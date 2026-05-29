mod assets;
mod sqlserver;
mod ollama;
mod providers;
mod routes;
mod sentinel_api;
mod logs;
mod scan;
mod health;

use axum::{routing::get, Router};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

const PREFERRED_PORT: u16 = 3690;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,backend=info".into()))
        .init();

    let cors = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(Any);

    let app = Router::new()
        .nest("/api", routes::router())
        .route("/assets/*path", get(assets::serve_path))
        .fallback(get(assets::serve))
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
    let no_open = std::env::var("SQLOPT_NO_OPEN").is_ok();
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
    eprintln!("{green}{bold}▣ sqlopt{reset} {dim}/ observatory{reset}");
    eprintln!("{dim}  SQL Server static + plan + DMV analyzer · Rust + WASM{reset}");
    eprintln!();
    eprintln!("  {bold}{green}  →  {url}  {reset}");
    eprintln!();
    eprintln!("{dim}  port {port}  ·  set SQLOPT_NO_OPEN=1 to skip auto-browser{reset}");
    eprintln!("{dim}  press ctrl-c to stop{reset}");
    eprintln!();
}
