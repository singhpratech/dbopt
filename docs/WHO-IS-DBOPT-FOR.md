# Who is dbopt for?

dbopt meets three roles where they already work. Each section below uses the same
frame — **Pain → Promise → Proof → Path** — so you can see exactly what it does for
*your* job and where to start.

| Role | The pain dbopt removes | Start here |
|---|---|---|
| **SQL Developer** | Hours lost scrolling a giant script; guessing which index to add | [§1](#1-sql-developer) — paste a script, no connection needed |
| **DBA** | "Is the server healthy? What regressed? What's hurting users?" | [§2](#2-dba) — Health front-door + Sentinel |
| **SysAdmin / Platform** | "Is it safe to run? What access does it need? How do I deploy it?" | [§3](#3-sysadmin--platform) — single binary, read-only |
| **Eng. Manager / Decision-maker** | "Will it actually help, and can I trust it?" | [§4](#4-engineering-manager--decision-maker) |

> dbopt is **free and open** — no per-seat cost, no paywalled features, no monetizing
> developer pain. "Adopt" below never means "buy."

---

## 1. SQL Developer
*"I only care about getting my indexes right, and I'm tired of a 3000-line script eating hours."*

**Pain.** A long stored-proc or migration script is a wall of text. You scan it line by line
for non-sargable predicates and missing indexes, then guess at `CREATE INDEX` statements and
hope they help.

**Promise.** dbopt turns "read 3000 lines for hours" into "review a ranked list in minutes,"
and it tells you the *exact* index to create — grounded in either the script's access pattern
or the server's real usage.

**Proof.**
- The static analyzer runs **100 rules** in milliseconds with a self-graded **F1 = 1.000** on our
  135-scenario corpus (covering 68 of those rules) — so the flags it's tested on are precise, not noise.
- On a real 82 KB / 3000-line script it produced **2,249 findings in 0.02 s**, then **grouped them
  into a handful of ranked cards** instead of a flat 2,249-row dump.

### How to optimize indexes — the workflow

**A. No connection (instant, works anywhere):**
1. Paste your script (or open a `.sql` file) into the analyzer.
2. Findings arrive **grouped by rule**, ranked by severity × count — e.g. *"12× non-sargable
   predicate (function on indexed column)"*, *"5× SELECT \* widening a covering index."*
3. For a large multi-object script, use the **Sections** selector to scope to a single
   procedure/function — counts and jump-chips recompute for just that section.
4. **Sort/filter** by severity, click a finding to **jump to the exact line**, hit **Copy fix**,
   or **"Ask AI to fix all N here →"** (the AI is sent that section's actual code, not a guess).

**B. With a connection (the real, data-backed recommendations):**
1. Connect (SQL auth is enough — see [ACCESS.md](ACCESS.md)).
2. Run **ADVISE**. dbopt reads live DMVs and returns **ranked, copy-paste `CREATE INDEX` DDL**:
   - **Missing indexes** the engine itself wants (from `sys.dm_db_missing_index_*`), ranked by
     measured impact × seeks × cost — with the key/included columns already filled in.
   - **Unused & duplicate indexes** to drop (write overhead with no read benefit), from real
     `sys.dm_db_index_usage_stats`.
   - Sizes/heatmap so you see where the wins actually are.
3. Every suggestion is **review-then-run** ("Safe-Apply") — dbopt never executes DDL for you.

**Net:** the "scroll for hours + guess the index" loop becomes "scoped, ranked findings →
copy-paste DDL grounded in evidence."

**Access you need:** Tier 0 for static; Tier 1–2 for live index advice. See [ACCESS.md](ACCESS.md).

---

## 2. DBA
*"Tell me what's hurting users and what regressed — across the server, over time."*

**Pain.** Stitching together DMV queries, Query Store, deadlock graphs and wait stats by hand,
repeatedly, just to answer "is it healthy?"

**Promise.** One **Health front-door** scores the database now; the **Sentinel** daemon watches
it continuously and writes a weekly pain report.

**Proof.**
- `POST /api/health/db` fuses the Advisor + static engine + Sentinel into a single ranked
  `Issue[]` with a **dual grade** (Reliability = active harm, Efficiency = wins available),
  each issue clickable through to its remediation.
- Sentinel polls Query Store, **wait deltas, deadlocks (`system_health`), live blocking, index
  usage, and sizes** into a local SQLite time-series — surfacing **regressed queries** and
  trends, not just a point-in-time snapshot.
- False-alarm classes (deadlock-count inflation, benign-wait noise) are already fixed.

**Path:** open **Health** → review the ranked issues and dual grade → start **Sentinel** for
continuous monitoring → download the weekly HTML/JSON report.

**Access you need:** Tier 2 (`VIEW SERVER STATE`) for full live signals; Tier 3 (+ Query Store)
for monitoring. See [ACCESS.md](ACCESS.md).

---

## 3. SysAdmin / Platform
*"Before I run this anywhere: is it safe, what does it touch, and how do I deploy it?"*

**Pain.** New tooling that needs `sa`, opens ports, reads data, or drags in a runtime/agent.

**Promise.** A **single, self-contained binary** that is **read-only**, **local-first**, and
**never reads your table data.**

**Proof.**
- One binary embeds the whole UI (rust-embed); no external services, no separate runtime.
  SQLite is bundled. Available for **Linux, macOS, Windows**.
- Binds to **127.0.0.1** only. Connects *out* to your SQL Server over TDS (default `1433`);
  needs **no inbound internet** to the database.
- **Read-only by design:** every query hits `sys.*` catalog views, DMVs, or Query Store, plus
  compile-only `SET SHOWPLAN_XML`. No DDL is ever executed; "Safe-Apply" only generates scripts
  for a human to run. **No `sysadmin` required.**
- DB connection errors are sanitized — credentials never appear in responses or logs.
- Degrades gracefully on least-privilege logins / Azure SQL (logs once and skips, never crashes).

**Path:** drop the binary on a workstation or jump host → create the **least-privilege login**
(copy-paste script in [ACCESS.md](ACCESS.md)) → run `./dbopt` and open `http://127.0.0.1:3690`.

**Deployment matrix & exact grants:** see [ACCESS.md](ACCESS.md). Works against self-managed SQL
Server 2019–2025, AWS RDS for SQL Server, and Azure SQL MI/DB.

---

## 4. Engineering Manager / Decision-maker
*"Will it help my team, and can I trust the output?"*

- **Trust:** verified live against SQL Server **2019, 2022, and 2025**; static-rule self-graded **F1 = 1.000**
  across 135 tagged scenarios (68 of 100 rules covered); **never reads your data**; never auto-executes changes.
- **Coverage:** version-aware 2019→2025 (a 2022 rewrite is never suggested against a 2019 target).
- **Adoption is low-friction:** a developer can get value with **zero database access** (static
  analysis) on day one, then opt into deeper, connection-backed analysis as access is granted.

### A systematic adoption path
1. **Try (0 access):** run static analysis on your worst stored proc / migration script.
2. **Pilot (read-only login):** point it at one non-prod database; review the ranked index advice.
3. **Roll out (monitoring):** enable Query Store + Sentinel on the databases that matter; share
   the weekly health report.

---

*See also: [ACCESS.md](ACCESS.md) for the exact permission tiers and least-privilege grant script.*
