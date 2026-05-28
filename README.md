# dbopt

**Find and fix slow SQL _before_ it reaches production — statically, privately, and prescriptively.**

dbopt is a local-first database performance optimizer. It reads your T-SQL, your
execution plans, and your live server metrics, then tells you exactly what's
going to hurt — and how to fix it, with the reasoning cited. Nothing leaves your
machine unless you choose to send it.

> **SQL Server is the product today** — comprehensive and fully tested.
> PostgreSQL and MySQL are on the roadmap (future, not yet implemented). The
> engine seam is in place so they slot in without disturbing the SQL Server core.

---

## Why this exists

Slow SQL is the silent tax on every data-heavy company: the query "that's been
running since last night," the 2 a.m. incident, the cloud bill that keeps
climbing. The usual ways to fight it are unsatisfying:

- **Expensive, SQL-Server-locked enterprise suites** that cost more than the problem.
- **Tools that only react _after_ a query runs** — by then the damage is done.
- **Raw DMV dumps** that tell you _what_ is slow but not _why_ or _what to do_.
- **Cloud SaaS** that wants you to ship your queries and schema off-box — a
  non-starter for pharma, finance, and healthcare.

dbopt takes the opposite stance on all four.

## What dbopt gives you

- **Shift-left analysis.** Catch the anti-pattern _before_ the query runs.
  dbopt analyzes statically and from the *estimated* plan — **no execution, no
  locks, no load on production.** It will happily dissect a query you'd never
  dare run. (We've pointed it at 100M+ row tables and optimized a multi-hour
  query without executing it once.)
- **Prescriptive + cited fixes.** Not just "here's a finding" — the concrete
  rewrite *and* the engine-level reasoning behind it. 52 rules, each with a
  recommendation.
- **Three lenses, one tool.** Static T-SQL analysis · execution-plan cost
  breakdown · live DMV + continuous monitoring. Most tools do one.
- **Local-first and private.** A single Rust binary. SQLite for storage. An
  optional **local** LLM (Ollama) for AI help. Your SQL and schema never leave
  the box unless you explicitly pick a cloud model.
- **Continuous sentinel.** A lightweight daemon polls your instance, builds a
  time-series, and surfaces a weekly **pain report** — top waits, regressions,
  unused indexes — so you catch trouble early instead of at 2 a.m.
- **Grounded AI assistant.** The assistant gets your SQL *and* the static
  findings injected as context, so it explains and rewrites with real grounding
  — and you can fan the same prompt out to several models to compare.

## Who it's for

DBAs and senior backend/data engineers — especially teams **without** a
dedicated performance expert, and regulated shops that **can't** send data to a
cloud service.

---

## How it works

dbopt looks at your workload through three complementary lenses:

1. **Static analysis** — a token-level T-SQL analyzer (52 rules across hygiene,
   sargability, deprecated syntax, modern rewrites, plan-shape, locking, tempdb,
   statistics, and index design). Runs in-browser via WebAssembly or as a native
   CLI. No connection required.
2. **Execution-plan analysis** — fetches the *estimated* plan (`SET SHOWPLAN_XML`,
   compile-only) and breaks down operator cost, scans vs. seeks, and spill risk.
3. **Live + continuous** — pulls DMVs (index usage, missing indexes, sizes) on
   demand, and the **sentinel** daemon polls Query Store, waits, deadlocks, live
   requests, index usage, and sizes into a local SQLite time-series.

Everything is version-aware (SQL Server 2014 → 2025): a 2022+ rewrite is never
suggested against a 2014 target.

## Quick start

```bash
# Build everything (single workspace, no external services required)
cargo build --release

# 1) Analyze a script statically — no DB connection needed
./target/release/sqlopt path/to/query.sql

# 2) Run the web observatory (serves the UI + API on :3690)
./target/release/sqlopt-backend
#    then open http://127.0.0.1:3690

# 3) Continuous monitoring (reads connection from env; SQL auth)
SQLOPT_SERVER="host,1433" SQLOPT_USER="..." SQLOPT_PASSWORD="..." \
  ./target/release/sqlopt-sentinel run

# 4) The rule-quality eval, with an HTML report
cargo run -p eval -- --html   # → target/eval-report.html
```

For UI development: `cd web && npm install && npm run dev` (proxies the API to
the backend on :3690).

## Architecture

A Rust workspace plus a React/Vite/TypeScript front end:

| Crate | Role |
|---|---|
| `analyzer-core` | the rule engine + tokenizer + plan/DMV models |
| `analyzer-wasm` | WebAssembly bindings for in-browser analysis |
| `analyzer-cli`  | `sqlopt` — analyze a `.sql` / `.sqlplan` / bundle from the shell |
| `backend`       | `sqlopt-backend` — axum API + embedded web UI, LLM proxy, durable logs |
| `sentinel`      | `sqlopt-sentinel` — continuous DMV poller → SQLite → pain report |
| `eval`          | the rule-quality harness (precision/recall/F1 + HTML report) |
| `web/`          | the "observatory" UI (analysis, plans, charts, AI, monitoring) |

Storage and config live under `~/.sqlopt/` (override with `SQLOPT_DATA_DIR`).
No external services are required to run dbopt.

## Quality bar

dbopt holds itself to a measurable accuracy target and proves it:

- **113 eval scenarios**, precision = recall = **F1 = 1.000** (target ≥ 0.95).
- **100% positive and 100% negative rule coverage** — every rule has a scenario
  that proves it fires when it should *and* stays silent when it shouldn't.
- Rust unit + HTTP integration tests and a Playwright UI suite —
  `cargo test --workspace` and `npm run test:e2e` both green.

Run `cargo run -p eval -- --html` and open the report to see the live board.

## Status & roadmap

**SQL Server (2014 → 2025) — the product. Complete and tested.** Static analysis,
estimated-plan analysis, live DMVs, continuous sentinel, AI assistant, web UI.
This is where the focus is and where it stays sharp.

**Future (roadmap, not yet started) — multi-engine.** PostgreSQL and MySQL are a
deliberate *later*. The `Engine` seam already exists (the analyzer accepts a
target engine and filters rules), so adding them never destabilizes the SQL
Server core. The abstraction that unlocks them:

- an `Engine` trait for connection, catalog/metric queries, plan capture, and
  version model (SQL Server's `sys.dm_*` / `SHOWPLAN_XML` → Postgres
  `pg_stat_*` / `EXPLAIN (FORMAT JSON)`, etc.);
- a per-rule engine tag (many rules are universal; some are dialect-specific);
- engine-parameterized API + UI.

---

dbopt is local-first by design: your queries, schema, and metrics stay on your
infrastructure. The web is at **dbopt.org**.
