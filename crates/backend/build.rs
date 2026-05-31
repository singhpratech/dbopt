//! Build-time guard for the embedded web UI.
//!
//! The backend embeds `web/dist/` at COMPILE time via rust-embed. That folder
//! is a build artifact (gitignored), so a fresh `git clone` has nothing to
//! embed — historically that produced either a hard compile error or a binary
//! that silently served only a placeholder, breaking every from-source install.
//!
//! This script makes the build deterministic: if a real bundle is present we
//! leave it alone; if it's missing we drop in a clearly-labelled placeholder
//! `index.html` so the embed always has content (the compile never fails), and
//! we print a prominent warning telling the builder exactly how to produce the
//! real UI. The release pipeline (`.github/workflows/release.yml`) builds the
//! real bundle and ships it inside every published artifact.
//!
//! We deliberately do NOT shell out to `npm`/`wasm-pack` here: a multi-minute
//! web build hidden inside `cargo build` would surprise contributors and CI
//! jobs that only want the Rust crates. The real build is one documented,
//! explicit sequence (see README "Quick start").

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // crates/backend -> ../../web/dist
    let dist = manifest_dir.join("../../web/dist");
    let index = dist.join("index.html");

    // Re-embed when the bundle changes (rebuilds the backend after `npm run
    // build`, which was previously an easy-to-forget manual step).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed={}", index.display());

    // Stamp the brand icon onto the Windows .exe (no-op off Windows). Done
    // before the UI-bundle early-return so it always runs.
    embed_windows_icon();

    // Decide whether a REAL bundle is present. A real Vite index.html can be
    // small, so we don't use size — we detect our own placeholder by an explicit
    // marker. Any index.html WITHOUT the marker is treated as a real (or
    // user-supplied) build and is never overwritten.
    let has_real_ui = fs::read_to_string(&index)
        .map(|s| !s.contains(PLACEHOLDER_MARKER) && s.trim().len() > 40)
        .unwrap_or(false);
    if has_real_ui {
        return;
    }

    // No bundle yet — guarantee the embed compiles and serves a useful message.
    if let Err(e) = fs::create_dir_all(&dist) {
        println!("cargo:warning=could not create {}: {e}", dist.display());
        return;
    }
    if fs::write(&index, PLACEHOLDER_INDEX).is_ok() {
        println!("cargo:warning=================================================================");
        println!("cargo:warning= dbopt: the web UI bundle (web/dist/) was not found.");
        println!("cargo:warning= This binary will serve a placeholder page until you build it.");
        println!("cargo:warning= To build the real UI from source, run from the repo root:");
        println!("cargo:warning=   wasm-pack build crates/analyzer-wasm --target web \\");
        println!("cargo:warning=        --out-dir ../../web/src/wasm --release");
        println!("cargo:warning=   cd web && npm install && npm run build && cd ..");
        println!("cargo:warning=   cargo build --release -p backend");
        println!("cargo:warning= (Published release artifacts already include the built UI.)");
        println!("cargo:warning=================================================================");
    }
}

// Embed the dbopt brand icon into the Windows executable so it shows up in
// Explorer, the taskbar, and the Start-menu shortcut instead of the generic
// default .exe icon. The `winresource` build-dep is target-gated to
// `cfg(windows)`, and this code path is host-gated to `cfg(windows)`. Our
// release pipeline builds the Windows binary on a Windows runner (host ==
// target == windows), so the icon is embedded there; Linux/macOS builds compile
// the no-op below and never reference `winresource`.
#[cfg(windows)]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=wix/dbopt.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("wix/dbopt.ico");
    if let Err(e) = res.compile() {
        // Don't fail the build over a cosmetic icon — warn and ship anyway.
        println!("cargo:warning=dbopt: could not embed Windows icon: {e}");
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}

/// Sentinel embedded in the placeholder so we can recognise (and safely
/// overwrite) our own page without ever clobbering a real bundle.
const PLACEHOLDER_MARKER: &str = "dbopt-placeholder-marker";

/// Minimal placeholder so rust-embed always has an index.html to embed. Kept in
/// sync with src/placeholder.html (the runtime fallback for an empty embed).
const PLACEHOLDER_INDEX: &str = r#"<!doctype html>
<!-- dbopt-placeholder-marker -->
<html lang="en"><head><meta charset="utf-8" />
<title>dbopt · build the UI</title>
<style>
  body{margin:0;background:#0a0d12;color:#d6dbe5;font:14px/1.6 ui-monospace,Menlo,monospace;
       display:grid;place-items:center;min-height:100vh;text-align:center;padding:20px;}
  .card{max-width:560px}
  h1{color:#d4ff4e;font-weight:500;letter-spacing:0.04em;margin:0 0 14px}
  code{background:#131822;padding:2px 6px;color:#d4ff4e;display:inline-block;margin:2px 0}
  a{color:#7fb4ff}
</style></head>
<body><div class="card">
  <h1>dbopt backend is running</h1>
  <p>The web UI hasn't been built into this binary yet. From the repo root:</p>
  <p><code>wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release</code></p>
  <p><code>cd web &amp;&amp; npm install &amp;&amp; npm run build &amp;&amp; cd ..</code></p>
  <p>then rebuild the backend (<code>cargo build --release -p backend</code>), or grab a
     published release that already includes the UI.</p>
  <p style="opacity:0.6;margin-top:30px">api: <a href="/api/health">/api/health</a></p>
</div></body></html>
"#;
