<p align="center"><img src="../web/public/logo.svg" width="72" height="72" alt="dbopt" /></p>

# dbopt — Usage guide

dbopt finds and fixes slow SQL **before** it reaches production. This guide walks the whole tool end to end: install → run → connect → each workspace → the CLI and the on-demand pulse poller.

> SQL Server (2014 → 2025) is the supported engine today. **Static analysis targets 2014 → 2025.** Live-connection features (Watch, Query Store capture, the VLF and instant-file-initialization health checks) use catalog objects introduced in 2016 and are skipped on 2014. Everything is **local-first** — your queries, schema and metrics never leave the machine unless you explicitly pick a cloud AI model.

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

Or download an installer from the [releases page](https://github.com/singhpratech/dbopt/releases) — `.msi`/`.zip` (Windows), `.dmg` (macOS), `.tar.gz` (glibc or static musl, Linux). Each is **one self-contained binary** with the web UI embedded — nothing else to install.

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
- **Auth** — SQL login (username + password) everywhere; **Windows builds support Windows/integrated auth out of the box** (no custom build needed). Kerberos *on Linux/macOS* is the one case that needs `--features integrated-auth`. The sentinel poller currently supports SQL logins only.
- **Database** — optional; leave blank for server-wide.

The connection is **server-level**: analysis runs both ad-hoc (a single script) and DB-wide (scan every database). Connections are saved as **named server profiles** so you can switch between instances. Passwords are only persisted if you tick *remember*.

> dbopt never reads your table rows. It reads catalog metadata, DMVs, Query Store, and **estimated** plans (compile-only). DDL is always preview-only (Safe-Apply) — it never auto-runs a change.

## 4. The workspaces

The left rail is grouped **START → OPERATE → INSPECT → SETUP**. Toggle **Developer / DBA** mode in the top bar to hide or show the server/operations surfaces.

### START
- **❤ Health** — the front door. One click fuses static analysis + advisor + monitor into a scored, ranked list of issues with a dual grade (Reliability / Efficiency). Each issue is clickable through to its remediation.
- **▤ Analyze** — paste or load T-SQL. The analyzer runs in-browser (WebAssembly) and lists findings with severity, the offending line, the **concrete rewrite**, and the engine-level reasoning. Buttons: **Check syntax** (parses on the connected server via `SET PARSEONLY ON` — syntax only, object names are not bound, so a missing table is *not* an error here; dbopt additionally flags a misspelled first keyword such as `SELCT 1`, which the server would otherwise accept as an implicit `EXEC SELCT`), **Estimated plan**, **Actual plan** (runs inside an always-rollback transaction; refuses DDL/EXEC/COMMIT). Set the target version (2014 / 2016 / 2017 / 2019 / 2022 / 2025) so rewrites are never suggested above your engine. *Offline index suggestions order the key columns by SARGable role (equality predicates before range/inequality), not by measured histogram selectivity — connect to confirm the most selective column leads.*
- **⌬ Connection** — manage server profiles (see §3).

### OPERATE
- **◉ Watch** — on-demand **Live Pulse**: real-time vitals (CPU load, throughput, contention, waits) polled from DMVs while you watch, plus **Report** mode that keeps the top-50 queries by duration every few minutes. This is the UI view of the **sentinel** poller (§6). You start it and read it yourself; it can also raise threshold alerts to a webhook (§6). There is no paging or escalation service.
- **✦ Advise** — turns DMV usage stats into ranked, copy-paste T-SQL recommendations (missing/unused/duplicate indexes, etc.). *Empty advisor ≠ broken* — SQL Server resets DMV stats on restart, so a freshly-restarted instance just hasn't accumulated stats yet. *Ranking caveat:* every usage-based recommendation (missing, unused and duplicate indexes, and their impact ranking) is computed from DMV counters that SQL Server resets on restart and on index rebuild. The ranking is only as good as the counter age: check the uptime shown beside the advice before trusting a #1, and treat a young-counter list as a hint, not a verdict.
- **⌖ Runs** — history of the analyses you ran from the **Analyze** editor (ad-hoc scripts, deduplicated by SQL hash). Health checks, database scans, plan fetches and `dbopt` CLI runs are **not** recorded here.
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
dbopt bundle.json                  # an AnalyzeInput JSON bundle (sql + plan_xml + dmv_bundle)
cat query.sql | dbopt --stdin      # read the script from stdin
dbopt-backend                      # the web observatory (API + UI on :3690)

# Lint a whole tree for CI (offline, no connection)
dbopt lint ./db --format human                  # pretty, grouped by file
dbopt lint ./db --format sarif > dbopt.sarif     # SARIF 2.1.0 → GitHub code scanning / VS Code SARIF Viewer
dbopt lint ./db --fail-on warning                # exit 1 on any finding ≥ warning (gates a PR)
```

`dbopt lint` recursively discovers `.sql` files, applies all 103 rules, and exits **0** clean /
**1** findings at-or-above `--fail-on` (default `error`) / **2** usage error. See the **Lint in CI**
section of the [README](../README.md) for a copy-paste GitHub Actions snippet.

## 6. On-demand pulse poller (sentinel)

The **sentinel** poller you start on demand samples Query Store, waits, deadlocks, live requests, index usage and sizes into a local SQLite time-series, and builds a **pain report** you read yourself. It also carries a threshold **alert engine** that posts to a webhook you configure (Slack, Teams, or a generic JSON endpoint). It is not a hands-off APM: alerts fire to your webhook and you triage them — there is no paging or escalation service.

```bash
# one-off poll of an instance
DBOPT_SERVER="host,1433" DBOPT_USER="sa" DBOPT_PASSWORD="…" dbopt-sentinel poll-once

# run continuously (^C to stop)
DBOPT_SERVER="host,1433" DBOPT_USER="sa" DBOPT_PASSWORD="…" dbopt-sentinel run

# render a markdown pain report from whatever data has been captured
dbopt-sentinel report 7            # last 7 days
```

The backend can also start/stop the sentinel from the **Watch** workspace and will **autostart** monitoring on boot if `~/.dbopt/sentinel-config.json` exists.

**Where the binary is:** `dbopt-sentinel` ships alongside `dbopt` and `dbopt-backend` in the `.tar.gz` (glibc and musl), the Windows `.zip` and the `.msi` (installed to the same `bin\` folder). The macOS `.dmg` and the Linux AppImage are launchers for the app only — take the `.tar.gz` if you want the command-line tools on those platforms. The pain report's headline wait uses the same benign-wait filter as Live Pulse, so a background scheduler wait never becomes the headline.

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
