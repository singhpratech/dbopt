# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**dbopt** — a local-first SQL Server performance optimizer. A Rust workspace (analyzer + axum backend
+ monitoring daemon) plus a React/Vite UI that ships *embedded inside the backend binary*. The repo
directory name is historical; the product, binaries, env vars and data dir are all `dbopt`.

Binaries: `dbopt` (CLI lint/analyze), `dbopt-backend` (API + UI on :3690), `dbopt-sentinel` (poller
daemon), `dbopt-eval` (rule-quality harness).

## Commands

```bash
# Full build (the web UI must be built FIRST — see "Embedded UI" below)
wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release
cd web && npm install && npm run build && cd ..
cargo build --workspace --release

# Tests
cargo test --workspace                       # unit + HTTP integration
cargo test -p dbopt-core rules::joins         # one module's tests
cargo test -p backend --test http_smoke       # spawns the real binary on :39123
cd web && npm run test:e2e                    # Playwright, system Chrome (channel:"chrome")
cd web && npx playwright test -g "WASM analyzer"   # one e2e spec

# Rule-quality eval (CI gate: F1 >= 0.95, currently held at 1.000)
cargo run -p eval                             # terminal report
cargo run -p eval -- --html                   # -> target/eval-report.html
cargo run -p eval -- --target 0.95 --json

# Run it
DBOPT_NO_OPEN=1 ./target/release/dbopt-backend   # :3690 (PORT= overrides; auto-opens browser otherwise)
cd web && npm run dev                             # :5173, proxies /api -> 127.0.0.1:3690

# CLI / CI linting
./target/release/dbopt lint ./db --format sarif --fail-on warning
./target/release/dbopt query.sql            # single-input JSON report (also .sqlplan / bundle .json / --stdin)

# Local SQL Server for testing (separate instance on :14333)
SA_PASSWORD='...' docker compose -f docker/sqlserver.yml up -d
```

CI (`.github/workflows/ci.yml`) runs `fmt` and `clippy` with `continue-on-error` — they are **not**
gates. The real gates are `cargo test --workspace --release` and the eval F1 target.

## Architecture

### One analyzer, three delivery paths
`analyzer-core::analyze(&AnalyzeInput) -> AnalysisReport` is the single entry point. `AnalyzeInput`
carries any combination of `sql`, `plan_xml`, `dmv_bundle`, `server_version`, `engine`; each populates
a different part of the report (token rules / plan treemap+findings / DMV charts + ranked
`recommendations` from `dmv::advise`). The same function is reached three ways:

- **Browser** — `analyzer-wasm` → `web/src/wasm-loader.ts`. The Analyze workspace runs fully offline.
- **Backend** — `POST /api/analyze`.
- **CLI** — the `dbopt` crate (dir `crates/analyzer-cli`), including `dbopt lint` with human/json/SARIF output.

Consequence: **any change to `analyzer-core` requires rebuilding the WASM bundle**, or the browser
silently keeps analyzing with the old rules while the CLI/backend use the new ones.

### Rules
Rule fns live in `crates/analyzer-core/src/rules/<family>.rs` with signature
`fn(&RuleCtx) -> Vec<Finding>`, and must be registered in `rules::REGISTRY` (`rules/mod.rs`) wrapped
in `ss(...)` — the engine-applicability preset. `run_all` skips rules whose `engines` don't include
the requested target, which is how the (currently inert) Postgres/MySQL `Engine` seam stays safe.
`REGISTRY` is the source of truth for the rule count quoted in the README/UI.

Two hard invariants:
- **The rule-id string is a public contract.** The eval matches `must_fire`/`must_not_fire` against
  the exact id emitted by `finding("family.rule_id", ...)`. Before writing a scenario, grep the real
  id: `grep -rn 'finding("' crates/analyzer-core/src/rules/<file>.rs`. A wrong id makes the guard
  vacuously pass.
- **Version-gate anything version-specific** via `ctx.server_version` (`if ctx.server_version.unwrap_or(0) < 2022 { return out }`).
  A 2022+ rewrite must never be suggested against a 2019 target.

### Eval corpus
`samples/scenarios/<id>/` — `query.sql` + `expected.json` (`must_fire`, `must_not_fire`,
`server_version`, `category`), optionally `plan.sqlplan` / `bundle.json`. Convention: `*_pos_NN` /
`*_neg_NN`, and **every rule needs both** a positive (fires) and a negative (stays silent on
superficially-similar benign SQL). A version-silence negative sets `server_version` below a rule's
gate. Because scenarios are hand-authored, the F1 number is *self-graded* — it proves "no regression
on the cases we wrote", not a real-world false-positive rate. Don't describe it otherwise in docs/UI.

### Backend (`crates/backend`)
axum, all routes under `/api` (`routes.rs`). Beyond analysis it hosts: the SQL Server client
(`sqlserver.rs`, tiberius), the **Health front door** (`health/`), on-demand live metrics, plan
fetch, and an LLM proxy (`providers/`, `ollama.rs`) for local + cloud models.

`health/mod.rs` is the aggregation layer worth reading before touching anything user-facing: it fuses
the DMV advisor + static findings + sentinel telemetry into one scored `HealthReport` behind a
`HealthProvider` trait, with three lanes (Reliability / Efficiency / Operational), a "learning" mode
for freshly-reset DMV counters, and `Issue`s the frontend renders without ever seeing DMV internals.
Adding an engine means implementing the trait, not changing the UI.

### Embedded UI (the #1 dev gotcha)
`assets.rs` embeds `web/dist/` at **compile time** via rust-embed. `web/dist/` and `web/src/wasm/`
are gitignored build artifacts, so `build.rs` writes a labelled placeholder `index.html` when they're
absent (keeps a fresh clone compiling) and prints the real build instructions.

So a UI change takes three steps, in order: `npm run build` → `cargo build -p backend` → **restart
the backend**. Editing `web/src` alone changes nothing about what the running binary serves (the Vite
dev server on :5173 is the fast path for UI iteration). `index.html` is served `no-store` and hashed
`/assets/` files are served immutable — a stale UI means you skipped a step, not a cache bug.

### Sentinel (`crates/sentinel`)
Per-surface pollers in `src/poll/` (query store, waits, deadlocks, index usage, CPU/memory/IO/tempdb
vitals, live) on independent cadences → SQLite at `~/.dbopt/sentinel.db` (`DBOPT_DATA_DIR` overrides)
→ weekly pain report + a threshold `alerts` engine with webhook `notify`. The backend can start,
stop and autostart it (`sentinel_api.rs` reads a persisted config on boot); it also runs standalone
via env vars only (`DBOPT_SERVER`, `DBOPT_USER`, `DBOPT_PASSWORD`, `DBOPT_DB`, `DBOPT_TRUST_CERT`,
`DBOPT_INSTANCE`) so secrets never appear in a process list.

### Web (`web/src`)
`App.tsx` owns `WORKSPACES` (left rail, grouped START → OPERATE → INSPECT → SETUP, with `dba?: true`
entries hidden in Developer mode). `api/backend.ts` is the only place that talks to `/api`;
`types.ts` mirrors the Rust serde wire shapes (snake_case) and must be updated alongside them. All
config/history persists to localStorage via `store/`. `glossary.ts` + `Term.tsx` exist so no acronym
ships unexplained; `confidence.ts` is the single vocabulary for observed / estimated / heuristic
trust tiers — import it rather than re-hardcoding glyphs.

## Project constraints

- **Read-only by design.** dbopt queries `sys.*` catalog views, DMVs and Query Store — never table
  rows. Estimated plans are compile-only (`SET SHOWPLAN_XML`); the actual-plan path runs inside an
  always-rollback transaction and refuses DDL/EXEC/COMMIT. DDL is preview-only (Safe-Apply never
  auto-runs). Keep new features inside this envelope.
- **Local-first, with one honest caveat.** No telemetry, no account, no upload. The *only* egress is
  a user-chosen cloud AI provider, which receives the prompt (SQL + findings). Never write "nothing
  leaves your machine" without that caveat — see `docs/DATA-HANDLING.md`.
- **No vendor-tool terminology in the UI.** Don't label anything after third-party/vendor tools
  (including chart labels); use the product's own vocabulary (CPU LOAD, THROUGHPUT, CONTENTION, …).
- **Windows auth:** tiberius's `winauth` feature is on by default and is target-gated to `cfg(windows)`
  inside the crate, so it's a no-op elsewhere — gate code on `cfg(windows)`, not on a feature flag.
  The separate `integrated-auth` (GSSAPI/Kerberos) feature is **off by default** and must stay off:
  it links system Kerberos libs that aren't present on every build host.
- **No external services.** Single binary; SQLite is bundled via rusqlite.
- Commits use a PII-free signature (`dbopt <dbopt@localhost>`). Don't push without being asked.
- `docs/` is the published site (dbopt.org) — README/docs claims are user-facing promises; verify
  against the code before changing a number or a capability statement.
