<p align="center">
  <img src="web/public/logo.svg" width="104" height="104" alt="dbopt" />
</p>

<h1 align="center">dbopt</h1>

<p align="center">
  <b>Find and fix slow SQL <i>before</i> it reaches production — statically, privately, prescriptively.</b>
</p>

<p align="center">
  <a href="https://github.com/singhpratech/dbopt/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/singhpratech/dbopt/ci.yml?branch=main&style=flat-square&labelColor=0a0d12&logoColor=white&label=ci&color=3ad29f&logo=githubactions" alt="CI" /></a>
  <a href="https://github.com/singhpratech/dbopt/actions/workflows/sql-lint.yml"><img src="https://img.shields.io/github/actions/workflow/status/singhpratech/dbopt/sql-lint.yml?branch=main&style=flat-square&labelColor=0a0d12&logoColor=white&label=sql-lint&color=3ad29f&logo=githubactions" alt="SQL lint" /></a>
  <a href="https://github.com/singhpratech/dbopt/releases/latest"><img src="https://img.shields.io/github/v/release/singhpratech/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&color=3c72ff&logo=github" alt="latest release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/singhpratech/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&color=7e879b&logo=apache" alt="Apache-2.0" /></a>
</p>
<!-- Re-enable the moment the packages are live on the registries:
  <a href="https://crates.io/crates/dbopt"><img src="https://img.shields.io/crates/v/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&label=crates.io&color=d4ff4e&logo=rust" alt="crates.io" /></a>
  <a href="https://www.npmjs.com/package/dbopt"><img src="https://img.shields.io/npm/v/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&label=npm&color=d4ff4e&logo=npm" alt="npm" /></a>
-->

<p align="center">
  <a href="https://dbopt.org"><b>Try it in your browser →</b></a>
  &nbsp;·&nbsp; <a href="docs/USAGE.md">Usage guide</a>
  &nbsp;·&nbsp; <a href="docs/WHO-IS-DBOPT-FOR.md">Who it's for</a>
  &nbsp;·&nbsp; <a href="docs/DATA-HANDLING.md">Data handling</a>
  &nbsp;·&nbsp; <a href="docs/ROADMAP.md">Roadmap</a>
</p>

---

**dbopt** is a **database performance optimizer**. Point it at a database and it reads your queries, your
execution plans and your live server metrics — then tells you exactly what is going to hurt and **how to
fix it, with the reasoning cited**. It works statically and from the *estimated* plan, so there is **no
execution, no locks, no load on production**.

> **One tool, every database.** dbopt is engine-agnostic from the core out: every rule declares which
> database it applies to, so engines are added without destabilizing each other. **SQL Server (2014 → 2025)
> is live today** with all 102 rules; **PostgreSQL and MySQL are next.** Ask for an engine whose rules
> haven't landed and you get an empty report — the analyzer would rather say nothing than guess.
>
> **Free and open.** No per-seat cost, no paywalled features — what the commercial tools do, without
> monetizing your pain.

## Try it without installing anything

Paste a query at **[dbopt.org](https://dbopt.org)** and the analyzer runs *in your browser* — it is this
repository's Rust engine compiled to WebAssembly, so your SQL never leaves the tab. The page measures and
prints its own network activity while you use it, so you don't have to take that on faith.

## Install

```bash
# Linux & macOS (Apple Silicon)
curl -fsSL https://dbopt.org/install.sh | sh
```
```powershell
# Windows (PowerShell)
irm https://dbopt.org/install.ps1 | iex
```

| Platform | Download |
|---|---|
| **Windows** (x64) | **[`.msi`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-windows-x86_64.msi)** · [portable `.zip`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-windows-x86_64.zip) |
| **macOS** (Apple Silicon) | **[`.dmg`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-macos-arm64.dmg)** |
| **Linux** (x64) | **[`.tar.gz`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-linux-x86_64.tar.gz)** (glibc 2.34+) · [static `musl`](https://github.com/singhpratech/dbopt/releases/latest/download/dbopt-linux-x86_64-musl.tar.gz) (Alpine, RHEL 8, any distro) |

Each is a **single self-contained binary** with the web UI embedded — run it, then open
`http://127.0.0.1:3690`. Checksums are on the [releases page](https://github.com/singhpratech/dbopt/releases).

**First run:** the builds are **not code-signed yet**, so the OS warns you once. Windows — *More info →
Run anyway*. macOS — *right-click → Open*, or `xattr -dr com.apple.quarantine /Applications/dbopt.app`.
Linux has no prompt. Signing is on the roadmap.

## Lint your SQL in CI — offline, no connection

`dbopt lint` walks your `.sql` files, applies all 102 rules and emits machine-readable output, so a bad
query fails the build *before* it ships.

```bash
dbopt lint ./db --format human               # grouped by file (default)
dbopt lint ./db --format json                # machine-readable findings
dbopt lint ./db --format sarif > dbopt.sarif # SARIF 2.1.0 for code scanning
dbopt lint ./db --fail-on warning            # exit 1 to gate a pull request
```

Exit codes: **0** clean · **1** findings at/above `--fail-on` (default `error`) · **2** usage error.

```yaml
- run: dbopt lint ./db --format sarif > dbopt.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with: { sarif_file: dbopt.sarif }
```

Findings then appear inline on the PR diff. The SARIF also opens in the VS Code
[SARIF Viewer](docs/EDITOR.md), and there is a [pre-commit hook](editor/hooks/pre-commit) that blocks a
commit on error-level findings.

## Three lenses, one tool

|   | Lens | What it does |
|---|------|--------------|
| **01** | **Static** | A token-level analyzer — **102 rules** across sargability, index design, plan shape, hygiene, modern rewrites, locking, tempdb, statistics, transactions, security and datatypes. Runs in-browser via WebAssembly or as a CLI. **No connection required.** |
| **02** | **Plan** | Fetches the *estimated* plan (compile-only — never runs the query) and breaks down operator cost, scans vs. seeks, spill and lookup risk. |
| **03** | **Live** | Reads index usage, missing indexes and sizes on demand; the **sentinel** daemon samples query history, waits, deadlocks and vitals into a local SQLite time-series, with thresholds and webhook alerts. |

Every finding carries a severity, the offending line, a **copy-paste rewrite** and the engine-level
reasoning behind it. Advice is **version-gated** — a 2022+ rewrite is never suggested against a 2019
target.

## Where it sits

|  | Free DBA scripts | Commercial monitors | dbopt |
|---|---|---|---|
| Cost | Free | Per-instance licence | **Free & open** |
| Works with no connection | No | No | **Yes** — static + plan |
| Catches it *before* it runs | No | No | **Yes** |
| Tells you what to type | Some advice | Metrics, rarely fixes | **Rewrite + reason** |
| Your data leaves the box | Never | Often a hosted service | **Never**, unless you pick a cloud model |
| Runs in CI | No | No | **SARIF, exit codes** |
| Cross-platform GUI | Vendor-tool bound | Windows-centric | **Linux, macOS, Windows** |
| 24/7 monitoring & paging | No | **Mature** | Early — capture, thresholds, webhooks |
| Years of production hardening | **Decades** | **Decades** | Young |

If you need a battle-tested 24/7 monitor with an on-call rotation behind it, buy one. dbopt is the
strongest option for catching the problem earlier, and for doing it without your queries leaving the
building.

## Local-first and private

A single **Rust binary** with SQLite for storage. dbopt reads catalog views, dynamic management views and
query history — **metadata, never your table rows**. Estimated plans are compile-only and DDL is
preview-only; Safe-Apply never runs a change for you.

**AI is your call.** Run a **local** model (Ollama / web-llm) and nothing leaves the machine. Prefer a
frontier model? Pick a cloud provider (Anthropic, OpenAI, Azure OpenAI, OpenRouter) and only your prompt —
the SQL plus its findings — is sent, and only when you choose it. Beyond that, the installed app makes one
optional anonymous version check to GitHub, which you can switch off. That is the complete list; see
[docs/DATA-HANDLING.md](docs/DATA-HANDLING.md). *(AWS Bedrock also works, but only in a source build with
the `bedrock` feature — it is not in the prebuilt downloads.)*

## Quality bar

- **152 eval scenarios** · precision = recall = **F1 = 1.000** (CI gate ≥ 0.95). The harness is
  **self-graded** — the scenarios are hand-authored, so this proves *no regression on the cases we wrote*,
  not a measured real-world false-positive rate. A held-out third-party corpus is on the roadmap.
- **75 of the 102 rules** currently have a scenario, each with a positive case (proves it fires) and a
  negative case (proves it stays silent). The newer rule packs are being backfilled.
- Rust unit + HTTP integration tests, and a Playwright UI suite.

```bash
cargo run -p eval -- --html   # → target/eval-report.html
```

## Engines

| Engine | Status | Notes |
|---|---|---|
| **SQL Server** | **Live** | 2014 → 2025 · all 102 rules |
| PostgreSQL | Next | engine seam wired · rules coming |
| MySQL | Next | engine seam wired · rules coming |

The `Engine` seam runs end to end — `AnalyzeInput` → `analyze()` → `rules::run_all` — and every rule
declares the engines it applies to, so each new database plugs in behind the same API, UI and report
without touching the ones already shipping. See [docs/ROADMAP.md](docs/ROADMAP.md).

## Architecture

A Rust workspace plus a React / Vite / TypeScript front end:

| Crate | Role |
|---|---|
| `dbopt-core` | the rule engine, tokenizer and plan / metric models (dir: `crates/analyzer-core`) |
| `dbopt` | the CLI — lint a tree, or analyze a `.sql` / `.sqlplan` / bundle (dir: `crates/analyzer-cli`) |
| `analyzer-wasm` | WebAssembly bindings for in-browser and Node analysis |
| `backend` | `dbopt-backend` — axum API + embedded web UI, LLM proxy, durable logs |
| `sentinel` | `dbopt-sentinel` — continuous poller → SQLite → pain report, alerts |
| `eval` | the rule-quality harness (precision / recall / F1 + HTML report) |
| `web/` | the "observatory" UI (analysis, plans, charts, AI, monitoring) |

Storage and config live under `~/.dbopt/` (override with `DBOPT_DATA_DIR`). No external services required.

## Build from source

You'll need Rust, Node 18+ and `wasm-pack`:

```bash
# The web UI is embedded into the backend binary at compile time, so build it first.
wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release
cd web && npm install && npm run build && cd ..

cargo build --release

./target/release/dbopt path/to/query.sql   # analyze a script, no connection needed
./target/release/dbopt-backend             # the web UI + API on :3690

DBOPT_SERVER="host,1433" DBOPT_USER="..." DBOPT_PASSWORD="..." \
  ./target/release/dbopt-sentinel run      # continuous monitoring
```

For UI work: `cd web && npm run dev` (proxies the API to the backend on :3690). Contributions —
especially new rules — are covered in [CONTRIBUTING.md](CONTRIBUTING.md).

## Authentication

SQL Server authentication (username + password) works out of the box, and Windows builds support
integrated Windows auth. For **Kerberos on Linux**, rebuild with the `integrated-auth` feature:

```bash
cargo build --release -p backend  --features integrated-auth
cargo build --release -p sentinel --features integrated-auth
```

It is off by default because those system libraries aren't on every build host.

---

<p align="center">
  <sub>Local-first by design — your queries, schema and metrics stay on your infrastructure.</sub><br>
  <sub><a href="https://dbopt.org">dbopt.org</a></sub><br>
  <sub>© 2026 <a href="https://theaivibe.org/about">Prateek Singh</a></sub>
</p>
