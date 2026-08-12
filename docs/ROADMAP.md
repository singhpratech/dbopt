# dbopt roadmap

dbopt is a local-first database performance optimizer built as one product for many
databases. SQL Server (2014 → 2025) is live today with all 103 rules; PostgreSQL and
MySQL are next. The engine seam is already wired end to end, so each database plugs in
behind the same API, UI and report without disturbing the ones already shipping.

## v0.1 — SQL Server (shipped)

- 103 token-level T-SQL rules (hygiene, sargability, deprecated, modern, plan-shape,
  locking, tempdb, statistics, transactions, security, datatypes, index design),
  version-gated 2014 → 2025.
- Estimated-plan analysis (`SET SHOWPLAN_XML`, compile-only — no execution, no locks).
- Live DMV pull (index usage, missing indexes, sizes) + `/api/scan/database` schema sweep.
- Sentinel: 6 DMV pollers → SQLite time-series → weekly pain report, with autostart-from-disk.
- AI assistant (local Ollama + cloud providers, fanout) grounded on the static findings.
- Web "observatory" UI; durable AI + analysis logs.
- Quality: 275 eval scenarios, self-graded F1 = 1.000 (covering all 104 rule ids
  plus 13 more for the 12 plan-XML/DMV checks); Rust unit +
  HTTP integration tests; Playwright UI e2e.

## The engine seam (landed)

`analyzer-core` now has an `Engine` enum (`SqlServer | Postgres | MySql`, default
`SqlServer`) that flows through `AnalyzeInput` → `analyze()` → `rules::run_all`.
Each rule in the `REGISTRY` declares the engine(s) it applies to (`Rule { run, engines }`),
and `run_all` skips rules that don't apply to the requested target. Today every rule
is tagged `SqlServer`; analyzing with `engine: "postgres"` correctly yields zero
findings until Postgres rules exist. That is the contract every future engine plugs
into.

## Next — PostgreSQL and MySQL

> SQL Server stays the priority while its analyzer deepens, but the other engines are
> planned work rather than speculation. No dates are promised. The engine seam already
> isolates this work so adding a database never destabilizes one that is shipping, and
> an engine whose rules have not landed returns an empty report rather than a guess.

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
Audit the 103 rules: many sargability/hygiene *concepts* are universal (leading
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
