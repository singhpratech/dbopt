# dbopt roadmap

dbopt is a local-first database performance optimizer focused on being the deepest
SQL Server static optimizer. SQL Server is the only supported engine today — 100% of
the rules are SQL-Server-specific. Other engines (Postgres, MySQL) are exploratory
directions, not committed releases or dates.

## v0.1 — SQL Server (shipped)

- 102 token-level T-SQL rules (hygiene, sargability, deprecated, modern, plan-shape,
  locking, tempdb, statistics, transactions, security, datatypes, index design),
  version-gated 2019 → 2025.
- Estimated-plan analysis (`SET SHOWPLAN_XML`, compile-only — no execution, no locks).
- Live DMV pull (index usage, missing indexes, sizes) + `/api/scan/database` schema sweep.
- Sentinel: 6 DMV pollers → SQLite time-series → weekly pain report, with autostart-from-disk.
- AI assistant (local Ollama + cloud providers, fanout) grounded on the static findings.
- Web "observatory" UI; durable AI + analysis logs.
- Quality: 147 eval scenarios, self-graded F1 = 1.000 (covering 73 of the 102 rules; newer
  packs still being backfilled); Rust unit + HTTP integration tests; Playwright UI e2e.

## The engine seam (landed — an exploratory foundation, not a committed multi-engine release)

`analyzer-core` now has an `Engine` enum (`SqlServer | Postgres | MySql`, default
`SqlServer`) that flows through `AnalyzeInput` → `analyze()` → `rules::run_all`.
Each rule in the `REGISTRY` declares the engine(s) it applies to (`Rule { run, engines }`),
and `run_all` skips rules that don't apply to the requested target. Today every rule
is tagged `SqlServer`; analyzing with `engine: "postgres"` correctly yields zero
findings until Postgres rules exist. This seam keeps the door open; it is not a
promise that other engines will ship.

## Exploratory (not committed) — multi-engine

> Deliberately *later* and explicitly **exploratory** — no committed release or
> date. SQL Server is the product and stays the priority; the items below are
> directions we *could* take, not promises, and do not begin until the SQL Server
> analyzer is where we want it. The engine seam already isolates this work so it
> never destabilizes the core.

### 1. `Engine` connection trait (the big one)
Abstract the live-server work that is currently SQL-Server-specific (tiberius +
`sys.dm_*` + `SHOWPLAN_XML`) behind a trait so other engines plug in:

```rust
#[async_trait]
trait DbEngine {
    fn kind(&self) -> Engine;
    async fn connect(&self, conn: &ConnectionInfo) -> Result<Conn>;
    async fn server_version(&self, c: &mut Conn) -> Result<EngineVersion>;
    async fn estimated_plan(&self, c: &mut Conn, sql: &str) -> Result<PlanModel>;
    async fn pull_metrics(&self, c: &mut Conn) -> Result<MetricBundle>; // index usage, sizes, missing idx
    async fn list_databases(&self, c: &mut Conn) -> Result<Vec<String>>;
    async fn enumerate_modules(&self, c: &mut Conn) -> Result<Vec<DbModule>>;
}
```

Mapping per engine:

| Concern        | SQL Server                         | PostgreSQL                          | MySQL                              |
|----------------|------------------------------------|-------------------------------------|------------------------------------|
| Driver         | tiberius                           | sqlx/tokio-postgres                 | sqlx/mysql_async                   |
| Plan capture   | `SET SHOWPLAN_XML ON`              | `EXPLAIN (FORMAT JSON)`             | `EXPLAIN FORMAT=JSON`              |
| Metrics        | `sys.dm_db_*`, Query Store         | `pg_stat_*`, `pg_statio_*`          | `performance_schema`, `sys.*`      |
| Missing index  | `sys.dm_db_missing_index_*`        | (heuristic from plan + pg_stat)     | (heuristic)                        |
| Version model  | 2019–2025 (`@@VERSION`)            | server_version_num                  | VERSION()                          |
| Catalog        | `sys.objects`/`sys.sql_modules`    | `information_schema`/`pg_proc`      | `information_schema`               |

### 2. Per-rule engine tags
Audit the 102 rules: many sargability/hygiene *concepts* are universal (leading
wildcard, function-on-column, SELECT *), even if the trigger syntax differs;
plan-shape/locking/tempdb rules are largely SQL-Server-specific. Tag accordingly
and add Postgres/MySQL rule families. Extend the eval corpus with an engine
dimension so coverage is tracked per engine.

### 3. Engine-parameterized API + UI
`/api/*` endpoints and the connection panel carry the target engine; the analyzer
and sentinel select the right `DbEngine` impl.


## Later
- Signed / notarized installers (builds are currently unsigned).
- Stats ascending-key + PSP detection refinements; columnstore advisor depth.
- Optional DuckDB-backed analytics over the sentinel time-series.
