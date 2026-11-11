use std::{error::Error, ffi::OsStr, path::PathBuf};
use tokio::net::TcpListener;

fn nonempty(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|v| !v.is_empty())
}

fn default_state_path(
    local_app_data: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, &'static str> {
    let data = nonempty(local_app_data)
        .or_else(|| nonempty(xdg_data_home))
        .map(PathBuf::from)
        .or_else(|| nonempty(home).map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("local app data directory unavailable; pass --state PATH")?;
    Ok(data.join("RepoConvoy").join("state.redb"))
}

fn usage() -> &'static str {
    "RepoConvoy local dashboard\nUsage: repoconvoy [--state PATH] [--port PORT]\n  --state PATH  redb state file (default: local app data/RepoConvoy/state.redb)\n  --port PORT   loopback HTTP port (default: 4317)\n  --help        print this help"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut state = None;
    let mut port: u16 = 4317;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--help" | "-h") => {
                println!("{}", usage());
                return Ok(());
            }
            Some("--state") => {
                state = Some(PathBuf::from(args.next().ok_or("--state requires a path")?))
            }
            Some("--port") => {
                port = args
                    .next()
                    .ok_or("--port requires a value")?
                    .to_str()
                    .ok_or("port must be numeric")?
                    .parse()
                    .map_err(|_| "port must be between 1 and 65535")?;
                if port == 0 {
                    return Err("port must be between 1 and 65535".into());
                }
            }
            _ => {
                return Err(format!(
                    "unknown option: {}\n{}",
                    argument.to_string_lossy(),
                    usage()
                )
                .into());
            }
        }
    }
    let state = match state {
        Some(path) => path,
        None => default_state_path(
            std::env::var_os("LOCALAPPDATA").as_deref(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )?,
    };
    if let Some(parent) = state.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!(
        "RepoConvoy: http://127.0.0.1:{port} (state: {})",
        state.display()
    );
    repoconvoy::serve(listener, state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn linux_default_state_uses_home_when_data_variables_are_missing() {
        let path = default_state_path(None, None, Some(OsStr::new("/home/alex"))).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/alex/.local/share/RepoConvoy/state.redb")
        );
    }

    #[test]
    fn explicit_data_home_takes_precedence_over_home() {
        let path = default_state_path(
            None,
            Some(OsStr::new("/xdg")),
            Some(OsStr::new("/home/alex")),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/xdg/RepoConvoy/state.redb"));
    }
}
