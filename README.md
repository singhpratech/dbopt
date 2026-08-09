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
  <img src="https://img.shields.io/badge/rules-102-d4ff4e?style=flat-square&labelColor=0a0d12" alt="102 rules" />
  <img src="https://img.shields.io/badge/eval%20F1-1.000-3ad29f?style=flat-square&labelColor=0a0d12" alt="F1 1.000" />
  <img src="https://img.shields.io/badge/local--first-by%20default-3ad29f?style=flat-square&labelColor=0a0d12" alt="local-first" />
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20React-7e879b?style=flat-square&labelColor=0a0d12" alt="Rust + React" />
</p>

<p align="center">
  <a href="https://dbopt.org"><b>dbopt.org</b></a>
  &nbsp;·&nbsp; <a href="docs/USAGE.md">Usage guide</a>
  &nbsp;·&nbsp; <a href="docs/WHO-IS-DBOPT-FOR.md">Who it's for</a>
  &nbsp;·&nbsp; <a href="docs/ACCESS.md">Access</a>
  &nbsp;·&nbsp; <a href="docs/ROADMAP.md">Roadmap</a>
</p>

---

**dbopt** is a **database performance optimizer**. Point it at a database and it reads your queries, your
execution plans, and your live server metrics — then tells you exactly what's going to hurt and **how to
fix it, with the reasoning cited**. It works statically and from the *estimated* plan, so there's **no
execution, no locks, no load on production**. Nothing leaves your machine unless you explicitly choose a
cloud model.

> **The deepest SQL Server static optimizer.** **SQL Server is the only supported engine today** (2019 → 2025) —
> 100% of the rules are SQL-Server-specific. The static analyzer and index advisor are ship-grade; on-demand
> capture (sentinel) and the health score are an early-warning telemetry layer you read yourself — not a
> 24/7 production monitor with alerting. PostgreSQL and MySQL are **exploratory directions, not committed
> releases** — the engine seam exists, but no other engine ships until the SQL Server core is where we want it.
>
> **Free and open.** No per-seat cost, no paywalled features — what the commercial tools do, without
> monetizing your pain.

---

## Download

**Install in one line** (no toolchain needed):

```bash
# Linux & macOS (Apple Silicon)
curl -fsSL https://dbopt.org/install.sh | sh
```
```powershell
# Windows (PowerShell)
irm https://dbopt.org/install.ps1 | iex
```

…or grab an installer directly:

| Platform | Download |
|---|---|
| **Windows** (x64) | **[`.msi`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-windows-x86_64.msi)** · [portable `.zip`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-windows-x86_64.zip) |
| **macOS** (Apple Silicon) | **[`.dmg`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-macos-arm64.dmg)** |
| **Linux** (x64) | **[`.tar.gz`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-linux-x86_64.tar.gz)** (glibc 2.34+) · [static `musl`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-linux-x86_64-musl.tar.gz) (runs on **any** Linux — Alpine, RHEL 8, old distros) · or the one-line installer above (adds a menu entry) |

Each is a **single self-contained binary** (the web UI is embedded) — run it, then open `http://127.0.0.1:3690`. New to dbopt? See the **[Usage guide](docs/USAGE.md)**. Checksums and all builds are on the [releases page](https://github.com/singhpratech/dbopt/releases).

### First run (current builds aren't code-signed yet)

dbopt is open-source and the builds are **not yet code-signed**, so the OS will warn you the first time. It's safe — verify the SHA256 against [`SHA256SUMS`](https://github.com/singhpratech/dbopt/releases/latest) if you like — and here's how to get past each prompt:

- **Windows** — SmartScreen shows *"Windows protected your PC."* Click **More info → Run anyway**.
- **macOS** — Gatekeeper says dbopt *"cannot be opened"* / *"is damaged."* **Right-click the app → Open → Open**, or run `xattr -dr com.apple.quarantine /Applications/dbopt.app`.
- **Linux** — no prompt; just `./dbopt` (or use the installer, which adds it to your apps menu).

Signed/notarized installers are on the roadmap.

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
| **01** | **Static** | A token-level T-SQL analyzer — **102 rules** across hygiene, sargability, deprecated syntax, modern rewrites, plan-shape, locking, tempdb, statistics, transactions, security and index design. Runs in-browser via WebAssembly or as a CLI. **No connection required.** |
| **02** | **Plan** | Fetches the *estimated* plan (`SET SHOWPLAN_XML`, compile-only — never runs the query) and breaks down operator cost, scans vs. seeks, and spill risk. |
| **03** | **Live** | Pulls DMVs (index usage, missing indexes, sizes) on demand, and the **sentinel** daemon polls Query Store, waits, deadlocks and index usage into a local SQLite time-series → a weekly **pain report**. |

### 🛠 Prescriptive &amp; cited fixes

Not just "here's a finding" — the concrete **rewrite** *and* the engine-level **reasoning** behind it.
Every rule ships a recommendation, and the grounded **AI assistant** gets your SQL *and* the findings as
context (fan one prompt out to several models to compare). Everything is **version-aware (2019 → 2025)** —
a 2022+ rewrite is never suggested against a 2019 target.

### 🔒 Local-first &amp; private

A single **Rust binary** with SQLite for storage. The analyzer never phones home — **no telemetry, no
account, no upload, ever** — so it'll happily dissect a query you'd never dare send anywhere. Estimated
plans are compile-only and DDL is preview-only (Safe-Apply never auto-runs a change).

**AI is your call.** Run a **local** model (Ollama / web-llm) and *nothing* leaves the machine. Want a
frontier model instead? Pick a **cloud** provider (Anthropic, OpenAI, Azure OpenAI, OpenRouter) and only
your prompt — the SQL plus its findings — is sent, and only when you choose it. The one egress is yours to
make. *(AWS Bedrock is also supported, but only in a source build compiled with the `bedrock` feature — it
is not included in the prebuilt downloads.)*

## Quality bar — proven, not promised

- **152 eval scenarios** · precision = recall = **F1 = 1.000** on the scenario
  set (target ≥ 0.95). The harness is **self-graded** — the scenarios are hand-authored, so this proves
  "no regression on the cases we wrote," not a measured real-world false-positive rate. A held-out,
  third-party corpus is on the roadmap.
- **75 distinct rules** are currently covered by a scenario; the newer rule packs are still being backfilled.
  Each covered rule has both a positive case (proves it fires) and a negative case (proves it stays silent).
- Rust unit + HTTP integration tests and a Playwright UI suite.

```bash
cargo run -p eval -- --html   # → target/eval-report.html  (the live board)
```

## Lint in CI (offline, no connection)

`dbopt lint` walks your `.sql` files, applies all 102 rules, and emits machine-readable
output — so a bad query fails the build *before* it ships. No database, no account, no upload.

```bash
dbopt lint ./db --format human                 # pretty, grouped by file (default)
dbopt lint ./db --format json                   # machine-readable findings
dbopt lint ./db --format sarif > dbopt.sarif     # SARIF 2.1.0 for code-scanning tools
dbopt lint ./db --fail-on warning                # exit 1 if any finding ≥ warning (gates a PR)
```

Exit codes: **0** clean · **1** findings at/above `--fail-on` (default `error`) · **2** usage error.

Wire it into GitHub Actions — findings show up inline in the PR via code scanning:

```yaml
- run: dbopt lint ./db --format sarif > dbopt.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with: { sarif_file: dbopt.sarif }
```

The SARIF also opens in the VS Code **SARIF Viewer** extension (findings land in the Problems panel).

## Build from source

Prefer to build it yourself (or hacking on dbopt)? You'll need Rust, Node 18+ and `wasm-pack`:

```bash
# Build the web UI first (it is embedded into the backend binary at compile time).
# Requires Node 18+ and wasm-pack (`cargo install wasm-pack`).
wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release
cd web && npm install && npm run build && cd ..

# Build the Rust workspace (single workspace, no external services needed).
cargo build --release

# 1) Analyze a script statically — no DB connection needed
./target/release/dbopt path/to/query.sql

# 2) Run the web observatory (UI + API on :3690)
./target/release/dbopt-backend          # then open http://127.0.0.1:3690

# 3) Continuous monitoring (SQL auth via env)
DBOPT_SERVER="host,1433" DBOPT_USER="..." DBOPT_PASSWORD="..." \
  ./target/release/dbopt-sentinel run
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
| `analyzer-cli`  | `dbopt` — analyze a `.sql` / `.sqlplan` / bundle from the shell |
| `backend`       | `dbopt-backend` — axum API + embedded web UI, LLM proxy, durable logs |
| `sentinel`      | `dbopt-sentinel` — continuous DMV poller → SQLite → pain report |
| `eval`          | the rule-quality harness (precision / recall / F1 + HTML report) |
| `web/`          | the "observatory" UI (analysis, plans, charts, AI, monitoring) |

Storage and config live under `~/.dbopt/` (override with `DBOPT_DATA_DIR`). No external services required.

## Roadmap — one brand, every engine

**SQL Server (2019 → 2025) is the product.** The **static analyzer + index advisor** are ship-grade
(102 rules, self-graded F1 = 1.000 on 145 scenarios covering 73 of them; estimated/actual plan analysis; read-only DMV advisor). **Live
capture (sentinel) + the health score** are included as an early-warning telemetry layer — a real
head-start, not yet a full production monitor with alerting/trending. AI assistant + web UI round it out.

**PostgreSQL and MySQL are a deliberate _later_.** The `Engine` seam already exists (the analyzer accepts a
target engine and filters rules), so adding them never destabilizes the SQL Server core — one master brand,
a small per-engine flavor tag (`dbopt · SQL Server` → `PostgreSQL` → `MySQL`).

---

<p align="center">
  <sub>Local-first by design — your queries, schema, and metrics stay on your infrastructure.</sub><br>
  <sub><a href="https://dbopt.org">dbopt.org</a></sub><br>
  <sub>© 2026 <a href="https://theaivibe.org/about">Prateek Singh</a></sub>
</p>
