# dbopt — Access & Permissions

> **What level of access does dbopt need to give you real results?**
> This page answers it precisely, so your DBA/security team can sign off before you roll it out.
> dbopt is **free and open** — this is about *access*, never licensing.

dbopt is **read-only**. It never executes DDL, never changes a setting, and — verifiably —
**never reads your table data.** Every query it runs is against system catalog views
(`sys.tables`, `sys.indexes`, …), dynamic management views (`sys.dm_*`), Query Store
(`sys.query_store_*`), or a compile-only execution plan (`SET SHOWPLAN_XML ON`). It reads
*metadata, statistics, query telemetry, and object definitions* — not the rows in your tables.

---

## What you get at each access level

| Tier | Access required | What dbopt can do | Result quality |
|---|---|---|---|
| **0 — None** | No database connection at all | Static T-SQL analysis of any script or stored-proc text (102 rules: sargability, plan shape, deprecated syntax, hygiene, transactions, security, index design…). Version-aware (2019→2025). | Full, instantly. Great for code review / CI. |
| **1 — Connect + read metadata** | A login that can connect, plus `VIEW DEFINITION` and `SHOWPLAN` | Reads object/index definitions; fetches **estimated** execution plans (compile-only, query is *not* run); database-scoped index analysis (usage, missing indexes, table sizes) where `VIEW DATABASE STATE` is granted. | Strong for a single database. |
| **2 — Server state**  ⭐ | Tier 1 **+ `VIEW SERVER STATE`** | Unlocks server-scoped signals: **wait statistics, top queries by duration, blocking, I/O, the full Advisor depth, and the Health score's reliability lane.** | **This is the "real, full picture" tier.** |
| **3 — Pulse poller** | Tier 2 **+ Query Store enabled** on each DB | The **Sentinel** poller (started on demand) samples Query Store, wait deltas, deadlocks (`system_health`), live blocking, index usage, and sizes into a local time-series → on-demand pain report + regression detection. (Data capture + report you read yourself — no paging or alerting.) | Trend/regression analysis over time. |

Without `VIEW SERVER STATE` (Tier 2) the tool **degrades gracefully** — it logs once and
skips the signals it can't see, rather than failing — but the live results are partial.
**For a true 100% live assessment, grant Tier 2 (and Tier 3 if you want the on-demand pulse poller and trend history).**

---

## The exact least-privilege grant (copy-paste)

A dedicated read-only login is the recommended setup. **No `sysadmin`, no `db_owner`, no data
read required.**

```sql
-- Server level
CREATE LOGIN dbopt_ro WITH PASSWORD = '<a strong password>';
GRANT VIEW SERVER STATE   TO dbopt_ro;   -- DMVs: waits, exec stats, index usage, missing indexes, Query Store, system_health
GRANT VIEW ANY DEFINITION TO dbopt_ro;   -- index/table metadata + stored-proc / view source text
GRANT VIEW ANY DATABASE   TO dbopt_ro;   -- enumerate databases for a server-wide scan

-- Per database you want estimated-plan analysis on
USE [YourDatabase];
CREATE USER dbopt_ro FOR LOGIN dbopt_ro;
GRANT SHOWPLAN TO dbopt_ro;              -- estimated execution plans (compile-only; nothing executes)
```

`VIEW SERVER STATE` implies `VIEW DATABASE STATE` on every database, so DB-scoped DMVs and
Query Store are covered by the single server grant. That login can read everything dbopt needs
and **nothing else** — it cannot see your table data, change anything, or run DDL.

---

## By deployment type

| Platform | Auth | Full results (Tier 2/3) achievable? | Notes |
|---|---|---|---|
| **Self-managed SQL Server 2019–2025** (Windows or Linux) | SQL auth ✅ *(verified live: 2019, 2022, 2025)*; Windows/integrated ⚠️ *(build with `--features integrated-auth`; needs an AD domain)* | **Yes — 100%.** | The primary, fully-tested target. |
| **AWS RDS for SQL Server** | SQL auth (RDS master user) ✅; AWS Managed AD ⚠️ | **Yes.** RDS is the *real* engine. The master user is not `sysadmin`, but it **can grant `VIEW SERVER STATE`** — which is all dbopt needs. `system_health` and Query Store are available. | Use the RDS endpoint `:1433`; trust-cert or the RDS CA bundle for TLS. Not yet live-tested by us, but engine-identical to the verified self-managed builds. |
| **Azure SQL Managed Instance** | SQL auth ✅; Entra ID (Azure AD) ❌ *(not yet supported)* | **Mostly yes.** Near-full instance; `VIEW SERVER STATE` is grantable. | Not yet live-tested. |
| **Azure SQL Database** (single DB / elastic pool) | SQL auth ✅; Entra ID ❌ | **Partial.** It's a different PaaS engine: some server-scoped DMVs don't exist (e.g. `sys.dm_os_wait_stats` → `sys.dm_db_wait_stats`), and there's no `system_health` session. DB-scoped index analysis + Query Store work. | Static analysis + per-DB advisor work; server-wide signals are limited by the platform, not dbopt. |

**Authentication summary:** SQL authentication (username + password) is the default and the
fully-tested path on every platform above. Windows/Kerberos integrated auth requires a build
with `--features integrated-auth` and a domain to authenticate against. Entra ID (Azure AD)
token auth is **not yet supported**.

---

## What dbopt will **never** ask for

- ❌ `sysadmin` / `sa` — not required on any platform.
- ❌ Permission to read your table **data** — it only reads metadata, stats, and query telemetry.
- ❌ Any write/DDL permission — "Safe-Apply" generates fix scripts for you to review and run
  yourself; dbopt never executes them.
- ❌ OS / filesystem access on the database host.
- ❌ Outbound internet from the database — dbopt runs locally and connects *to* your SQL Server.

---

## 100%-results checklist

To get the complete, real assessment a buyer is evaluating:

1. ☐ A login with **`VIEW SERVER STATE`** + **`VIEW ANY DEFINITION`** (server level).
2. ☐ **`SHOWPLAN`** granted on each database you want plan-level analysis for.
3. ☐ Network reachability from where you run dbopt to the SQL Server on its TDS port (default `1433`).
4. ☐ TLS trusted (enable *trust server certificate*, or install the server/RDS CA).
5. ☐ *(For continuous monitoring)* **Query Store enabled** on each monitored database
   (`ALTER DATABASE [db] SET QUERY_STORE = ON;`).

Tiers 0 and 1 work with less; everything above unlocks the full live picture.
