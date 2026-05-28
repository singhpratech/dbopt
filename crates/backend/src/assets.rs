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
    // Serve the exact file, else fall back to index.html for client-side routing.
    let (data, served) = match WebDist::get(candidate) {
        Some(f) => (f.data.into_owned(), candidate),
        None => match WebDist::get("index.html") {
            Some(f) => (f.data.into_owned(), "index.html"),
            None => return placeholder(),
        },
    };
    let mime = mime_guess::from_path(served).first_or_octet_stream();
    // index.html is the pointer to content-hashed bundles, so it must NEVER be
    // cached — a stale copy keeps loading an old bundle (the "nothing changed"
    // trap). The hashed files under /assets/ are immutable (name changes every
    // build), so they cache for a year. Everything else gets a short TTL.
    let cache = if served == "index.html" {
        "no-store, must-revalidate"
    } else if served.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300"
    };
    return Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(data))
        .unwrap();
}

/// No bundle baked in (dev mode). Hand back a friendly message, not a blank 404.
fn placeholder() -> Response {
    let html = include_str!("./placeholder.html");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(html))
        .unwrap()
}

/// Convenience handler for `/assets/...` paths (the way Vite ships them).
pub async fn serve_path(Path(path): Path<String>) -> Response {
    let uri = format!("/assets/{path}").parse::<Uri>().unwrap();
    serve(uri).await
}
