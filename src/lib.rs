use std::{error::Error, path::PathBuf};

use axum::{
    Router,
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use redb::Database;
use std::sync::Arc;
use tokio::net::TcpListener;
mod store;

pub async fn serve(
    listener: TcpListener,
    state_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let db = Arc::new(Database::create(state_path)?);
    let port = listener.local_addr()?.port();
    let app = Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [
                        (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    ],
                    axum::response::Html(include_str!("../web/dashboard.html")),
                )
            }),
        )
        .route(
            "/theme.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_bytes!("../web/theme.css"),
                )
            }),
        )
        .route(
            "/theme.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_bytes!("../web/theme.js"),
                )
            }),
        )
        .route(
            "/icons.svg",
            get(|| async { asset("image/svg+xml", include_bytes!("../web/icons.svg")) }),
        )
        .route(
            "/assets/app-icon.svg",
            get(|| async { asset("image/svg+xml", include_bytes!("../assets/app-icon.svg")) }),
        )
        .route(
            "/api/projects",
            get(store::list_projects).post(store::create_project),
        )
        .route(
            "/api/projects/{id}/sessions",
            get(store::list_sessions).post(store::create_session),
        )
        .route("/api/sessions/{id}/board", get(store::get_board))
        .route(
            "/api/capabilities",
            get(|| async { axum::Json(serde_json::json!({"dispatch": false})) }),
        )
        .with_state(db)
        .layer(middleware::from_fn(move |request, next| async move {
            guard_loopback_request(request, next, port).await
        }));
    axum::serve(listener, app).await?;
    Ok(())
}

async fn guard_loopback_request(
    request: Request<axum::body::Body>,
    next: Next,
    port: u16,
) -> Response {
    let headers = request.headers();
    let host = headers.get_all(header::HOST);
    let mut hosts = host.iter();
    let host = hosts.next().and_then(|value| value.to_str().ok());
    let allowed_host = host.is_some_and(|value| {
        value.eq_ignore_ascii_case(&format!("localhost:{port}"))
            || value == format!("127.0.0.1:{port}")
    }) && hosts.next().is_none();
    let allowed_origin = headers.get_all(header::ORIGIN).iter().all(|value| {
        value.to_str().ok().is_some_and(|origin| {
            host.is_some_and(|host| origin.eq_ignore_ascii_case(&format!("http://{host}")))
        })
    });
    let allowed_fetch_site = headers
        .get_all("sec-fetch-site")
        .iter()
        .all(|value| value.to_str().ok().is_some_and(|site| site != "cross-site"));
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = if !allowed_host || !allowed_origin || !allowed_fetch_site {
        StatusCode::FORBIDDEN.into_response()
    } else {
        next.run(request).await
    };
    if is_api {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response
            .headers_mut()
            .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    }
    response
}

fn asset(content_type: &'static str, body: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
}
