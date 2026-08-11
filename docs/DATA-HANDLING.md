# What dbopt does with your data (the honest version)

This is a plain, verified account of every place your data goes. If anything here
is ever untrue, it's a bug — please open an issue.

## TL;DR

- **The analyzer is local-first.** Connecting, scanning, grading, and monitoring
  talk **only** to your SQL Server and store results **only** on your own machine.
- **There is no telemetry, no analytics, no phone-home.** dbopt has no server of
  its own to call.
- **The one way your data can leave your machine is if *you* enable a CLOUD AI
  provider.** Then the prompt you send (which can include your SQL, schema, and
  findings) goes to that provider. Local AI models do not.

## What we read from your SQL Server

dbopt queries **catalog views and Dynamic Management Views only** — metadata,
not your rows:

- `sys.indexes`, `sys.tables`, `sys.index_columns`, `sys.partitions` — schema +
  index layout + row counts
- `sys.dm_db_index_usage_stats`, `sys.dm_db_missing_index_*` — index usage/needs
- `sys.dm_os_*`, `sys.configurations`, `sys.databases`, `sys.dm_db_log_info` —
  server/database config + log structure
- `msdb` backup history — last full/log backup times
- `sys.sql_modules` — the **source text of your stored procedures and views**
  (this is your code, not table data — disclosed for completeness)

**We never run `SELECT` against your application tables. Your row data is never
read.** (See ACCESS.md for the least-privilege grants this needs.)

## Where it's stored — all local

- **Monitoring telemetry** (waits, blocking, deadlocks, query-store stats, sizes)
  → `~/.dbopt/sentinel.db`, a SQLite file on your disk. Retained ~90 days, then
  pruned. Nothing is uploaded.
- **Connection profiles, LLM settings, drafts, AI logs** → your **browser's
  localStorage** (and `~/.dbopt` for the sentinel config). On-disk secrets are
  written `0600`.

## What can leave your machine

Two outbound internet paths exist, and **both are things you choose**: cloud AI
providers (below), and a manual "Check for updates" click (further below). With
neither in use, dbopt talks **only** to your SQL Server.

### Cloud AI providers

The AI features are the main outbound path. There are two kinds:

| Provider | Where it runs | Does your prompt leave? |
|---|---|---|
| **Ollama** | your machine (localhost) | **No** |
| **web-llm** | in your browser (WASM) | **No** |
| **OpenAI / Anthropic / Azure OpenAI / OpenRouter** | the vendor's cloud | **Yes** |
| **AWS Bedrock** (source builds with the `bedrock` feature only) | the vendor's cloud | **Yes** |

When you use a **cloud** provider, your prompt — which may contain your SQL,
schema, and the findings dbopt produced — is sent over HTTPS to that vendor, along
with your API key to authenticate. Your key is stored only in your browser and is
forwarded through the local backend to the vendor you chose; it is never sent to
any dbopt server (there isn't one).

**If your SQL must never leave the machine, use a local model (Ollama or
web-llm).** The deterministic analyzer/health/scan never needs AI at all — AI only
rephrases findings the engine already produced.

### The update check (Help ▸ Check for updates)

dbopt can tell you when a newer release exists. By default this fires **twice**:
once automatically when the app launches, and whenever you click "Check for
updates" in the Help panel. **The launch check is on by default and you can turn
it off** — untick "Check automatically on launch" in the update dialog, or set
`auto_check_updates` to false in local storage. Turning it off leaves the manual
button working.

The request is an anonymous public `GET` from your **browser** to `api.github.com`
(the dbopt releases endpoint). It carries no identifiers, no telemetry, and no
body — nothing about your databases, queries, or config is sent. What GitHub can
see is what any web request reveals: your IP address and the time. The local
backend is not involved in this call. The code is one self-contained file:
`web/src/api/updates.ts`.

If you need an environment with **zero** unattended egress, turn the launch check
off before you connect anything; after that, the only remaining outbound path is a
cloud AI provider you pick yourself.

## How to verify this yourself

- Watch the network: with only the analyzer in use (no cloud AI, launch update
  check off), the backend makes outbound connections **only** to your SQL Server.
- Grep the source: the only `https://` egress in the backend is in
  `crates/backend/src/providers/*` and `ollama.rs` — i.e. the AI providers. The
  update check is browser-side and lives only in `web/src/api/updates.ts`.
- There are no analytics/tracking libraries anywhere in the codebase.
