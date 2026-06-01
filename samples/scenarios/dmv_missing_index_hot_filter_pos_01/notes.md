# ts.missing_index_dmv — hot filter column (DMV)

Grounded in the real-world scenario surfaced by research: `sys.dm_db_missing_index_details`
/ `_group_stats` reports a high-value recommendation for a frequently-run query that
filters on equality `CustomerID` with no usable nonclustered index, so the engine
repeats clustered-index scans under load.

The bundle carries SQL Server's own DMV numbers (impact 96.7%, 24,180 seeks, avg cost
14.6) on a 2019 instance. `dmv::analyze` echoes this back as the informational
`dmv.missing_index` finding; the prescriptive `advise()` layer ranks it by the standard
improvement measure (`avg_total_user_cost * (avg_user_impact/100) * user_seeks`) and emits
a covering-index DDL (equality key first, SELECT-list columns in INCLUDE).

Reference verdict: a genuine missing index — build the equality column in the key and the
remaining query columns as INCLUDE to make it covering. Caveat (MS Learn): the missing-index
DMVs are heuristics, clear on restart, and cap at 600 rows, so this is a candidate to
validate against the real workload, not a command.

Sources:
- https://learn.microsoft.com/en-us/sql/relational-databases/indexes/tune-nonclustered-missing-index-suggestions
- https://www.brentozar.com/blitzindex/ (improvement_measure ranking)

NOTE — eval-harness scope: the other six research scenarios (ts.checkdb_never_run,
ts.hadr_secondary_lagging, ts.backup_job_failing_silently, ts.single_tempdb_file_ifi_off,
ts.dangerous_global_trace_flag, ts.parameter_sniffing) are LIVE operational checks that run
in the backend `health::operational` evaluator against `OperationalFacts`, not through
`analyzer_core::analyze`. The eval harness only drives `analyze()` (SQL + plan XML + index
DMV bundle), so those scenarios are validated by the operational unit tests, not here. They
are NOT a rule gap — every one maps to an implemented + unit-tested rule
(integrity.checkdb_never/checkdb_stale, hadr.replica_unhealthy, jobs.recent_failures,
tempdb.too_few_files + config.ifi_disabled, config.dangerous_trace_flag, and the
param-sniffing regression enrichment).
