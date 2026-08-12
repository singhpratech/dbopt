mod assets;
mod sqlserver;
mod ollama;
mod providers;
mod routes;
mod sentinel_api;
mod logs;
mod scan;
mod health;

use axum::{
    extract::Request,
    http::{HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

/// Is this an `Origin` we are willing to accept a state-changing request from?
/// dbopt binds to loopback, so the only legitimate origins are the embedded UI
/// (same-origin) and the Vite dev server.
fn is_loopback_origin(origin: &HeaderValue) -> bool {
    origin
        .to_str()
        .map(|o| {
            o.starts_with("http://localhost:")
                || o.starts_with("http://127.0.0.1:")
                || o.starts_with("http://[::1]:")
                || o == "http://localhost"
                || o == "http://127.0.0.1"
        })
        .unwrap_or(false)
}

/// Reject cross-origin state-changing requests.
///
/// CORS alone does not do this. A browser will happily *send* a cross-origin
/// POST and only withhold the response from the calling script, so any page the
/// user visited could POST to `/api/shutdown` or `/api/sentinel/stop` and
/// succeed — it never needed to read the reply. Endpoints that carry a JSON
/// body are protected incidentally (the content-type forces a preflight), which
/// is exactly why the body-less ones were the hole.
///
/// A browser always attaches `Origin` to a cross-origin request, so requests
/// with no `Origin` at all (curl, scripts, the CLI) are left alone.
async fn block_cross_origin_writes(req: Request, next: Next) -> Response {
    let mutating = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if mutating {
        if let Some(origin) = req.headers().get(axum::http::header::ORIGIN) {
            if !is_loopback_origin(origin) {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({
                        "error": "cross-origin requests cannot change state on this server"
                    })),
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}

const PREFERRED_PORT: u16 = 3690;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,backend=info".into()))
        .init();

    // Loopback origins only. dbopt binds to 127.0.0.1 and holds live database
    // credentials, so `allow_origin(Any)` meant any page the user happened to
    // visit could talk to it from their browser. The embedded UI is same-origin
    // and unaffected; this exists for the Vite dev server on :5173.
    // Loopback origins only. dbopt binds to 127.0.0.1 and holds live database
    // credentials, so `allow_origin(Any)` meant any page the user happened to
    // visit could read responses from it. The embedded UI is same-origin and
    // unaffected; this exists for the Vite dev server on :5173. Note that CORS
    // governs who may *read* a response — `block_cross_origin_writes` below is
    // what stops a foreign page from causing an effect it never needs to read.
    let cors = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(tower_http::cors::AllowOrigin::predicate(
            |origin: &HeaderValue, _req| is_loopback_origin(origin),
        ));

    let app = Router::new()
        .nest("/api", routes::router())
        .route("/assets/*path", get(assets::serve_path))
        .fallback(get(assets::serve))
        .layer(middleware::from_fn(block_cross_origin_writes))
        .layer(cors);

    // Choose a port: env override > preferred 3690 > next free.
    let port = match std::env::var("PORT").ok().and_then(|s| s.parse::<u16>().ok()) {
        Some(p) => p,
        None => {
            if port_is_free(PREFERRED_PORT) { PREFERRED_PORT }
            else {
                let fallback = portpicker::pick_unused_port().unwrap_or(0);
                tracing::warn!("port {PREFERRED_PORT} is busy, falling back to {fallback}");
                fallback
            }
        }
    };
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let url = format!("http://{addr}");

    print_banner(&url, port);

    // Open the user's browser (best effort) once the listener is ready.
    let url_for_open = url.clone();
    let no_open = std::env::var("DBOPT_NO_OPEN").is_ok();
    tokio::spawn(async move {
        // Tiny delay so axum is definitely accepting before we hand the URL to the browser.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if !no_open {
            let _ = open::that_detached(&url_for_open);
        }
    });

    // Resume continuous monitoring if a persisted sentinel config requested it.
    // Best-effort: failures just log and the server still starts.
    sentinel_api::autostart_from_disk().await;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn port_is_free(p: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
}

fn print_banner(url: &str, port: u16) {
    // ANSI bold + chartreuse where supported, plain otherwise.
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let green = "\x1b[38;2;212;255;78m";
    let reset = "\x1b[0m";
    eprintln!();
    eprintln!("{green}{bold}▣ dbopt{reset} {dim}/ observatory{reset}");
    eprintln!("{dim}  SQL Server static + plan + DMV analyzer · Rust + WASM{reset}");
    eprintln!();
    eprintln!("  {bold}{green}  →  {url}  {reset}");
    eprintln!();
    eprintln!("{dim}  port {port}  ·  set DBOPT_NO_OPEN=1 to skip auto-browser{reset}");
    eprintln!("{dim}  press ctrl-c to stop{reset}");
    eprintln!();
}
