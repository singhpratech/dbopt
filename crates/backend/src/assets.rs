//! Embeds the built web UI (`web/dist/`) and serves it from the same
//! axum router. If the directory hasn't been built yet (dev mode), the
//! routes still resolve but return a clear "build first" page.

use axum::{
    body::Body,
    extract::Path,
    http::{header, StatusCode, Uri},
    response::Response,
};
use rust_embed::RustEmbed;

// `#[folder]` is resolved relative to CARGO_MANIFEST_DIR by rust-embed,
// so this points at the project-root `web/dist/`.
#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebDist;

/// Serve `/` and any nested static path. Falls back to `index.html`
/// so client-side routing keeps working.
pub async fn serve(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let candidate = if raw.is_empty() { "index.html" } else { raw };
    if let Some(file) = WebDist::get(candidate).or_else(|| WebDist::get("index.html")) {
        let mime = mime_guess::from_path(candidate).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .header(header::CACHE_CONTROL, "public, max-age=300")
            .body(Body::from(file.data.into_owned()))
            .unwrap();
    }
    // No bundle baked in. Hand back a friendly message instead of a blank 404.
    let html = include_str!("./placeholder.html");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap()
}

/// Convenience handler for `/assets/...` paths (the way Vite ships them).
pub async fn serve_path(Path(path): Path<String>) -> Response {
    let uri = format!("/assets/{path}").parse::<Uri>().unwrap();
    serve(uri).await
}
