#!/usr/bin/env bash
# Regenerate the npm package's WebAssembly builds.
#
# npm/node/ and npm/web/ are build artifacts (gitignored), so a fresh clone has
# nothing to publish until this runs. Both targets come from the same crate:
# `nodejs` is synchronous CommonJS for Node, `web` is ESM that instantiates the
# .wasm on first use in a browser. package.json's exports map picks between them.
set -euo pipefail
cd "$(dirname "$0")/.."

# Strip absolute build paths out of the .wasm. Panic-location strings from
# dependencies otherwise embed the build machine's home directory — and the
# browser build is re-served to every end user who bundles this package.
# (`trim-paths` in Cargo.toml would be tidier but still needs nightly here.)
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=${PWD}=/dbopt"

wasm-pack build crates/analyzer-wasm --target nodejs --out-dir ../../npm/node --release
wasm-pack build crates/analyzer-wasm --target web    --out-dir ../../npm/web  --release

# wasm-pack writes its own package.json/README/.gitignore into each out-dir;
# ours live one level up and must win.
rm -f npm/node/package.json npm/node/README.md npm/node/.gitignore
rm -f npm/web/package.json  npm/web/README.md  npm/web/.gitignore

node -e "
  const { analyze } = require('./npm/node/index.cjs');
  const n = analyze('SELECT * FROM T WHERE YEAR(d)=2025;').findings.length;
  if (!n) { console.error('smoke test found no findings'); process.exit(1); }
  console.log('smoke test ok:', n, 'findings');
"
echo 'npm package ready — publish with:  cd npm && npm publish --access public'
