//! Controller-owned Git worktree candidates. This is separation by Git worktree,
//! not a security sandbox: a worker with filesystem access can still edit a source.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug)]
pub struct WorkspaceError(String);
impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for WorkspaceError {}
type Result<T> = std::result::Result<T, WorkspaceError>;
fn error(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError(message.into())
}

fn git(dir: &Path, args: &[&OsStr]) -> Result<Output> {
    let mut command = Command::new("git");
    command.current_dir(dir).args(args);
    // Parent processes (notably Git hooks) can export repository-routing state.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .output()
        .map_err(|e| error(format!("git launch: {e}")))
}
fn git_ok(dir: &Path, args: &[&OsStr]) -> Result<String> {
    let output = git(dir, args)?;
    if !output.status.success() {
        return Err(error(format!(
            "git failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|e| error(format!("git output is not UTF-8: {e}")))
}
fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    git_ok(dir, &args.iter().map(OsStr::new).collect::<Vec<_>>()).map(|text| text.trim().to_owned())
}
fn git_directory(dir: &Path, flag: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git_text(dir, &["rev-parse", flag])?);
    canonical(&if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}
fn canonical(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().map_err(|e| error(e.to_string()))?;
    // std::fs::canonicalize on Windows yields \\?\ paths, which Git for Windows
    // does not accept as worktree paths. Retain canonical identity without that prefix.
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(path)
}
fn canonical_future(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return canonical(path);
    }
    let parent = path.parent().ok_or_else(|| error("output has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| error("output has no name"))?;
    Ok(canonical_future(parent)?.join(name))
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn clean(dir: &Path) -> Result<bool> {
    Ok(git_text(dir, &["status", "--porcelain", "--untracked-files=all"])?.is_empty())
}

/// A controller holds at most one candidate for each registered source.
pub struct Workspace {
    root: PathBuf,
    candidates: BTreeMap<PathBuf, Candidate>,
}
impl Workspace {
    /// The controller directory is not created until a valid source is prepared.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        if root
            .as_ref()
            .components()
            .any(|part| part == Component::ParentDir)
        {
            return Err(error("controller output must not contain parent traversal"));
        }
        let root = canonical_future(root.as_ref())?;
        Ok(Self {
            root,
            candidates: BTreeMap::new(),
        })
    }

    pub fn candidate(&self, source: impl AsRef<Path>) -> Result<&Candidate> {
        let source = canonical(source.as_ref())?;
        self.candidates
            .get(&source)
            .ok_or_else(|| error("source is not registered"))
    }

    pub fn prepare(&mut self, source: impl AsRef<Path>) -> Result<&Candidate> {
        let source = canonical(source.as_ref())?;
        let top = PathBuf::from(git_text(&source, &["rev-parse", "--show-toplevel"])?);
        if canonical(&top)? != source {
            return Err(error("source must be the Git top-level directory"));
        }
        let base_sha = git_text(&source, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if !clean(&source)? {
            return Err(error("source has uncommitted or untracked changes"));
        }
        if overlaps(&source, &self.root) {
            return Err(error("controller output overlaps source"));
        }
        if self.candidates.keys().any(|other| overlaps(other, &source)) {
            return Err(error(
                "source already registered or overlaps registered source",
            ));
        }
        fs::create_dir_all(&self.root).map_err(|e| error(format!("controller: {e}")))?;
        // Atomic, persistent reservation: even another Workspace instance using this
        // controller cannot hand a second worker the same source. The marker stays
        // until the controller directory/worktree is explicitly retired.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in source.to_string_lossy().as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        let reservation = self.root.join(format!("source-{hash:016x}.lock"));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&reservation)
            .map_err(|e| error(format!("source already reserved in controller: {e}")))?;
        // A pre-existing path, symlink, or branch is never reused or overwritten.
        let allocation = (|| -> Result<(String, PathBuf)> {
            let mut index = 1_u64;
            let (branch, path) = loop {
                let label = format!("candidate-{index:06}");
                let branch = format!("repoconvoy/{label}");
                let path = self.root.join(&label);
                let refname = format!("refs/heads/{branch}");
                let reference_exists = git(
                    &source,
                    &[
                        OsStr::new("show-ref"),
                        OsStr::new("--verify"),
                        OsStr::new("--quiet"),
                        OsStr::new(&refname),
                    ],
                )?;
                if !path.exists() && !path.is_symlink() && !reference_exists.status.success() {
                    break (branch, path);
                }
                index = index
                    .checked_add(1)
                    .ok_or_else(|| error("candidate index exhausted"))?;
            };
            git_ok(
                &source,
                &[
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("-b"),
                    OsStr::new(&branch),
                    path.as_os_str(),
                    OsStr::new(&base_sha),
                ],
            )?;
            Ok((branch, path))
        })();
        // A partial Git failure keeps the reservation, failing closed until
        // an operator explicitly inspects and retires the candidate.
        let (branch, path) = allocation?;
        let common_dir = git_directory(&source, "--git-common-dir")?;
        if git_directory(&path, "--git-common-dir")? != common_dir {
            return Err(error("candidate does not belong to source repository"));
        }
        let git_dir = git_directory(&path, "--git-dir")?;
        let candidate = Candidate {
            source: source.clone(),
            base_sha,
            branch,
            path,
            common_dir,
            git_dir,
        };
        self.candidates.insert(source.clone(), candidate);
        Ok(self.candidates.get(&source).expect("just inserted"))
    }
}

pub struct Candidate {
    source: PathBuf,
    base_sha: String,
    branch: String,
    path: PathBuf,
    common_dir: PathBuf,
    git_dir: PathBuf,
}
impl Candidate {
    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn base_sha(&self) -> &str {
        &self.base_sha
    }

    /// Requires a clean committed descendant with a nonempty net diff from base.
    pub fn ready(&self) -> Result<ReadyCandidate> {
        if canonical(&self.path)? != self.path
            || canonical(&PathBuf::from(git_text(
                &self.path,
                &["rev-parse", "--show-toplevel"],
            )?))?
                != self.path
        {
            return Err(error("candidate path is not its original worktree"));
        }
        if git_directory(&self.source, "--git-common-dir")? != self.common_dir
            || git_directory(&self.path, "--git-common-dir")? != self.common_dir
            || git_directory(&self.path, "--git-dir")? != self.git_dir
        {
            return Err(error("candidate is not the registered source worktree"));
        }
        if git_text(&self.source, &["rev-parse", "HEAD"])? != self.base_sha {
            return Err(error("source HEAD changed since preparation"));
        }
        if !clean(&self.source)? {
            return Err(error("source has uncommitted or untracked changes"));
        }
        if git_text(&self.path, &["symbolic-ref", "--short", "HEAD"])? != self.branch {
            return Err(error("candidate branch changed"));
        }
        if !clean(&self.path)? {
            return Err(error("candidate has uncommitted or untracked changes"));
        }
        let head_sha = git_text(&self.path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if head_sha == self.base_sha {
            return Err(error("candidate has no new commit"));
        }
        let ancestry = git(
            &self.path,
            &[
                OsStr::new("merge-base"),
                OsStr::new("--is-ancestor"),
                OsStr::new(&self.base_sha),
                OsStr::new(&head_sha),
            ],
        )?;
        if !ancestry.status.success() {
            return Err(error("candidate HEAD does not descend from base"));
        }
        let diff = git_ok(
            &self.path,
            &[
                OsStr::new("diff"),
                OsStr::new("--no-ext-diff"),
                OsStr::new("--binary"),
                OsStr::new(&self.base_sha),
                OsStr::new(&head_sha),
                OsStr::new("--"),
            ],
        )?;
        if diff.is_empty() {
            return Err(error("candidate has no net change from base"));
        }
        Ok(ReadyCandidate { head_sha, diff })
    }
}

pub struct ReadyCandidate {
    pub head_sha: String,
    pub diff: String,
}
