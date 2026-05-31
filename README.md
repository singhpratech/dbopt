<p align="center">
  <img src="web/public/logo.svg" width="104" height="104" alt="dbopt" />
</p>

<h1 align="center">dbopt</h1>

<p align="center">
  <b>Find and fix slow SQL <i>before</i> it reaches production — statically, privately, prescriptively.</b>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-free%20%26%20open-d4ff4e?style=flat-square&labelColor=0a0d12" alt="free & open" />
  <img src="https://img.shields.io/badge/SQL%20Server-2019%20%E2%86%92%202025-3c72ff?style=flat-square&labelColor=0a0d12" alt="SQL Server 2019 to 2025" />
  <img src="https://img.shields.io/badge/rules-59-d4ff4e?style=flat-square&labelColor=0a0d12" alt="59 rules" />
  <img src="https://img.shields.io/badge/eval%20F1-1.000-3ad29f?style=flat-square&labelColor=0a0d12" alt="F1 1.000" />
  <img src="https://img.shields.io/badge/local--first-no%20cloud-3ad29f?style=flat-square&labelColor=0a0d12" alt="local-first" />
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20React-7e879b?style=flat-square&labelColor=0a0d12" alt="Rust + React" />
</p>

<p align="center">
  <a href="https://dbopt.org"><b>dbopt.org</b></a>
  &nbsp;·&nbsp; <a href="docs/WHO-IS-DBOPT-FOR.md">Who it's for</a>
  &nbsp;·&nbsp; <a href="docs/ACCESS.md">Access &amp; permissions</a>
  &nbsp;·&nbsp; <a href="docs/ROADMAP-TO-COMPLETE.md">Roadmap</a>
</p>

---

**dbopt** is a **database performance optimizer**. Point it at a database and it reads your queries, your
execution plans, and your live server metrics — then tells you exactly what's going to hurt and **how to
fix it, with the reasoning cited**. It works statically and from the *estimated* plan, so there's **no
execution, no locks, no load on production**. Nothing leaves your machine unless you explicitly choose a
cloud model.

> **One product, many engines.** **SQL Server is the first engine** — comprehensive and fully tested
> (2019 → 2025). PostgreSQL and MySQL are next; the engine seam is already in place, so each new database is
> a *flavor* of the same tool, not a separate product.
>
> **Free and open.** No per-seat cost, no paywalled features — what the commercial tools do, without
> monetizing your pain.

---

## Why it exists

Slow SQL is the silent tax on every data-heavy company: the query "that's been running since last night,"
the 2 a.m. incident, the cloud bill that keeps climbing. The usual fixes disappoint —

- **Expensive, SQL-Server-locked enterprise suites** that cost more than the problem.
- **Tools that only react _after_ a query runs** — by then the damage is done.
- **Raw DMV dumps** that tell you *what* is slow but not *why* or *what to do*.
- **Cloud SaaS** that wants your queries and schema off-box — a non-starter for pharma, finance, healthcare.

dbopt takes the opposite stance on all four.

## What you get

### 🔭 Three lenses, one tool

|   | Lens | What it does |
|---|------|--------------|
| **01** | **Static** | A token-level T-SQL analyzer — **59 rules** across hygiene, sargability, deprecated syntax, modern rewrites, plan-shape, locking, tempdb, statistics and index design. Runs in-browser via WebAssembly or as a CLI. **No connection required.** |
| **02** | **Plan** | Fetches the *estimated* plan (`SET SHOWPLAN_XML`, compile-only — never runs the query) and breaks down operator cost, scans vs. seeks, and spill risk. |
| **03** | **Live** | Pulls DMVs (index usage, missing indexes, sizes) on demand, and the **sentinel** daemon polls Query Store, waits, deadlocks and index usage into a local SQLite time-series → a weekly **pain report**. |

### 🛠 Prescriptive &amp; cited fixes

Not just "here's a finding" — the concrete **rewrite** *and* the engine-level **reasoning** behind it.
Every rule ships a recommendation, and the grounded **AI assistant** gets your SQL *and* the findings as
context (fan one prompt out to several models to compare). Everything is **version-aware (2019 → 2025)** —
a 2022+ rewrite is never suggested against a 2019 target.

### 🔒 Local-first &amp; private

A single **Rust binary**. SQLite for storage. An optional **local** LLM (Ollama). No telemetry, no account,
no upload — it'll happily dissect a query you'd never dare run. Estimated plans are compile-only and DDL is
preview-only (Safe-Apply never auto-runs a change).

## Quality bar — proven, not promised

- **135 eval scenarios** · precision = recall = **F1 = 1.000** (target ≥ 0.95).
- **100% positive and 100% negative coverage** — every rule has a scenario that proves it fires when it
  should *and* stays silent when it shouldn't.
- Rust unit + HTTP integration tests and a Playwright UI suite.

```bash
cargo run -p eval -- --html   # → target/eval-report.html  (the live board)
```

## Quick start

```bash
# Build the web UI first (it is embedded into the backend binary at compile time).
# Requires Node 18+ and wasm-pack (`cargo install wasm-pack`).
wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release
cd web && npm install && npm run build && cd ..

# Build the Rust workspace (single workspace, no external services needed).
cargo build --release

# 1) Analyze a script statically — no DB connection needed
./target/release/sqlopt path/to/query.sql

# 2) Run the web observatory (UI + API on :3690)
./target/release/sqlopt-backend          # then open http://127.0.0.1:3690

# 3) Continuous monitoring (SQL auth via env)
SQLOPT_SERVER="host,1433" SQLOPT_USER="..." SQLOPT_PASSWORD="..." \
  ./target/release/sqlopt-sentinel run
```

For UI development: `cd web && npm install && npm run dev` (proxies the API to the backend on :3690).

## Authentication

SQL Server authentication (username + password) works out of the box. For **Windows / integrated
(Kerberos) auth**, rebuild with the `integrated-auth` feature (links GSSAPI on Linux):

```bash
cargo build --release -p backend  --features integrated-auth
cargo build --release -p sentinel --features integrated-auth
```

It's off by default because those system libraries aren't on every build host (and aren't used on Windows
targets). With the feature on and no username/password supplied, dbopt uses the current Windows identity.

## Architecture

A Rust workspace plus a React / Vite / TypeScript front end:

| Crate | Role |
|---|---|
| `analyzer-core` | the rule engine + tokenizer + plan/DMV models |
| `analyzer-wasm` | WebAssembly bindings for in-browser analysis |
| `analyzer-cli`  | `sqlopt` — analyze a `.sql` / `.sqlplan` / bundle from the shell |
| `backend`       | `sqlopt-backend` — axum API + embedded web UI, LLM proxy, durable logs |
| `sentinel`      | `sqlopt-sentinel` — continuous DMV poller → SQLite → pain report |
| `eval`          | the rule-quality harness (precision / recall / F1 + HTML report) |
| `web/`          | the "observatory" UI (analysis, plans, charts, AI, monitoring) |

Storage and config live under `~/.sqlopt/` (override with `SQLOPT_DATA_DIR`). No external services required.

## Roadmap — one brand, every engine

**SQL Server (2019 → 2025) is the product** — complete and tested: static analysis, estimated-plan
analysis, live DMVs, continuous sentinel, AI assistant, web UI.

**PostgreSQL and MySQL are a deliberate _later_.** The `Engine` seam already exists (the analyzer accepts a
target engine and filters rules), so adding them never destabilizes the SQL Server core — one master brand,
a small per-engine flavor tag (`dbopt · SQL Server` → `PostgreSQL` → `MySQL`).

---

<p align="center">
  <sub>Local-first by design — your queries, schema, and metrics stay on your infrastructure.</sub><br>
  <sub><a href="https://dbopt.org">dbopt.org</a></sub><br>
  <sub>© 2026 <a href="https://theaivibe.org">Prateek Singh</a></sub>
</p>
