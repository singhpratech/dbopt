<p align="center"><img src="../web/public/logo.svg" width="72" height="72" alt="dbopt" /></p>

# dbopt — Usage guide

dbopt finds and fixes slow SQL **before** it reaches production. This guide walks the whole tool end to end: install → run → connect → each workspace → the CLI and the continuous monitor.

> SQL Server (2019 → 2025) is the supported engine today. Everything is **local-first** — your queries, schema and metrics never leave the machine unless you explicitly pick a cloud AI model.

---

## 1. Install

```bash
# Linux & macOS (Apple Silicon)
curl -fsSL https://dbopt.org/install.sh | sh
```
```powershell
# Windows (PowerShell)
irm https://dbopt.org/install.ps1 | iex
```

Or download an installer from the [releases page](https://github.com/singhpratech/dbopt/releases) — `.msi`/`.zip` (Windows), `.dmg` (macOS), `.AppImage`/`.tar.gz` (Linux). Each is **one self-contained binary** with the web UI embedded — nothing else to install.

## 2. Run the observatory

```bash
dbopt-backend          # starts the API + web UI on http://127.0.0.1:3690
```

Open **http://127.0.0.1:3690**. The first run shows a short onboarding, then drops you on the **Health** front door. Set `DBOPT_NO_OPEN=1` to stop it auto-opening a browser.

You can also analyze a script with **no server at all**:

```bash
dbopt path/to/query.sql            # static analysis of a .sql file
dbopt plan.sqlplan                 # break down a saved execution plan
```

## 3. Connect to a database

Open **Connection** (or the onboarding prompt) and enter:

- **Server** — `host,1433` (or `host\instance`)
- **Auth** — SQL login (username + password) out of the box. For Windows/Kerberos, build with the `integrated-auth` feature.
- **Database** — optional; leave blank for server-wide.

The connection is **server-level**: analysis runs both ad-hoc (a single script) and DB-wide (scan every database). Connections are saved as **named server profiles** so you can switch between instances. Passwords are only persisted if you tick *remember*.

> dbopt never reads your table rows. It reads catalog metadata, DMVs, Query Store, and **estimated** plans (compile-only). DDL is always preview-only (Safe-Apply) — it never auto-runs a change.

## 4. The workspaces

The left rail is grouped **START → OPERATE → INSPECT → SETUP**. Toggle **Developer / DBA** mode in the top bar to hide or show the server/operations surfaces.

### START
- **❤ Health** — the front door. One click fuses static analysis + advisor + monitor into a scored, ranked list of issues with a dual grade (Reliability / Efficiency). Each issue is clickable through to its remediation.
- **▤ Analyze** — paste or load T-SQL. The analyzer runs in-browser (WebAssembly) and lists findings with severity, the offending line, the **concrete rewrite**, and the engine-level reasoning. Buttons: **Check syntax** (real `SET PARSEONLY`), **Estimated plan**, **Actual plan** (runs inside an always-rollback transaction; refuses DDL/EXEC/COMMIT). Set the target version (2019 / 2022 / 2025) so rewrites are never suggested above your engine.
- **⌬ Connection** — manage server profiles (see §3).

### OPERATE
- **◉ Watch** — live vitals (CPU load, throughput, contention, waits) polled from DMVs, plus **Report** mode that keeps the top-50 queries by duration every few minutes. This is the UI view of the **sentinel** monitor (§6).
- **✦ Advise** — turns DMV usage stats into ranked, copy-paste T-SQL recommendations (missing/unused/duplicate indexes, etc.). *Empty advisor ≠ broken* — SQL Server resets DMV stats on restart, so a freshly-restarted instance just hasn't accumulated stats yet.
- **⌖ Runs** — history of your analysis runs.
- **⎯ Logs** — durable AI + analysis history (also downloadable as JSON/CSV). Persisted in `~/.dbopt/sentinel.db`.

### INSPECT
- **◫ Plan** — the estimated/actual execution plan as a cost treemap (operator cost, scans vs. seeks, spills).
- **◰ Index** — index-usage heatmap from live DMVs (seeks/scans/lookups/updates, missing indexes).
- **◧ Size** — table/index size treemap.
- **≡ Severity** — findings rolled up by severity.

### SETUP
- **↪ AI** — a grounded assistant. It receives your SQL **and** the static findings as context, so it explains and rewrites with real grounding. Run **one model** or **fan out** the same prompt to several providers side by side. Answers render as rich Markdown (tables, code blocks with a COPY button). Providers: local **Ollama** + **web-llm**, or cloud (Anthropic, OpenAI, Azure OpenAI, OpenRouter). **Only the cloud providers send your prompt off-box** — local models keep everything on the machine. *(AWS Bedrock is available only in a source build compiled with the `bedrock` feature, not in the prebuilt downloads.)*
- **⚙ Config** — theme, mode, providers, and version defaults. Everything persists to local storage.

## 5. The CLI

```bash
dbopt query.sql                    # static findings for a script
dbopt plan.sqlplan                 # analyze a saved plan
dbopt bundle.zip                   # a script + plan bundle
dbopt-backend                      # the web observatory (API + UI on :3690)
```

## 6. Continuous monitoring (sentinel)

The **sentinel** daemon polls Query Store, waits, deadlocks, live requests, index usage and sizes into a local SQLite time-series, and builds a weekly **pain report**.

```bash
# one-off poll of an instance
DBOPT_SERVER="host,1433" DBOPT_USER="sa" DBOPT_PASSWORD="…" dbopt-sentinel poll-once

# run continuously (^C to stop)
DBOPT_SERVER="host,1433" DBOPT_USER="sa" DBOPT_PASSWORD="…" dbopt-sentinel run

# render a markdown pain report from whatever data has been captured
dbopt-sentinel report 7            # last 7 days
```

The backend can also start/stop the sentinel from the **Watch** workspace and will **autostart** monitoring on boot if `~/.dbopt/sentinel-config.json` exists.

## 7. Environment variables

| Variable | Purpose | Default |
|---|---|---|
| `DBOPT_SERVER` | `host[,port]` | `localhost,1433` |
| `DBOPT_USER` / `DBOPT_PASSWORD` | SQL login | — |
| `DBOPT_DB` | database name | (server-wide) |
| `DBOPT_TRUST_CERT` | `1` to skip TLS validation | `1` |
| `DBOPT_INSTANCE` | display name for the instance | = server |
| `DBOPT_DATA_DIR` | where `sentinel.db` + config live | `~/.dbopt` |
| `DBOPT_NO_OPEN` | `1` to not auto-open the browser | — |

Storage and config live under **`~/.dbopt/`**. (Upgrading from the old `sqlopt` build? Your settings and monitoring data migrate automatically on first run.)

## 8. Data handling

- **Local-only:** no phone-home, no account, no telemetry to us. The analyzer never reads table rows.
- **The one egress:** cloud AI providers — and only when *you* pick a cloud model — receive your prompt (your SQL + findings). Local models (Ollama / web-llm) send nothing off-box.
- See **[DATA-HANDLING.md](DATA-HANDLING.md)** for the full breakdown.

---

Questions or a rule that misfired? Open an issue at [github.com/singhpratech/dbopt](https://github.com/singhpratech/dbopt). · [dbopt.org](https://dbopt.org)
