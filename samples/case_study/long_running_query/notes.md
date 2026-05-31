# Case study: the 10-hour sales report

A senior dev shipped `dbo.GetGmailSReport` six months ago. It was fine in
staging; in production it now runs for **hours**. The DBA team has been
managing it by killing the session, throwing more `WITH (NOLOCK)` at it,
adding `OPTION (RECOMPILE)`, and rebuilding indexes nightly. None of that
helps because none of the surface-level fixes address what the optimizer
is actually doing.

This case study walks the query end to end: every anti-pattern, the
mechanism by which it hurts, and the rewrite that lets the engine seek
instead of scan.

## What the analyzer flags

Running `dbopt-eval` (or the live web UI) over `02_baseline_query.sql`
should surface at least these rules:

| Rule | Severity | Why |
|------|----------|-----|
| `hygiene.unbounded_dml` | — | (not present here, but the analyzer's UPDATE catch covers DML in sibling procs) |
| `hygiene.select_star` | warning | `SELECT *` over a join of five wide tables — read amplification + breaks any chance of a covering index plan. |
| `hygiene.nolock` | error | `WITH (NOLOCK)` on four tables. Dirty reads, duplicated rows, missed rows, rows from rolled-back txns. |
| `modern.missing_schema_prefix` | info | Tables referenced as `Customers`, not `dbo.Customers`. Plan cache fragments per default-schema. |
| `modern.missing_set_nocount` | info | Proc body lacks `SET NOCOUNT ON;`. |
| `sarg.function_on_column` | error | `UPPER(c.LastName) = 'SMITH'` — non-SARGable. Optimizer cannot seek the `LastName` index. |
| `sarg.leading_wildcard` | warning | `Email LIKE '%@gmail.com'` — index seek is impossible. |
| `sarg.scalar_udf_in_predicate` | error/warning | `dbo.fnFullName(...)` in WHERE. Pre-2019 forces row-by-row + serial plan; 2019+ may inline but only conditionally — verify the plan. |
| `sarg.implicit_convert_unicode` | warning | `c.LastName = N'Smith'` — `LastName` is varchar, so the column is implicitly converted to nvarchar per-row. |
| `sarg.or_chain` | info | `Status = 1 OR Status = 2 OR Status = 3 OR Status = 4 OR Status = 5` — collapse to `IN (1,2,3,4,5)`. |
| `deprecated.lob_legacy_types` | warning | `AuditLog.Payload` is `text`. SARGable string ops can't apply to it. |

## What the rewrite changes

1. **NOLOCK → RCSI.** `docker/bootstrap/00_init.sql` enables `READ_COMMITTED_SNAPSHOT`. The rewrite drops every hint and relies on row-versioning for non-blocking reads.

2. **`UPPER(LastName) = 'SMITH'` → `LastName = 'Smith'`.** Default Windows collation is case-insensitive (`SQL_Latin1_General_CP1_CI_AS`). The `UPPER` was load-bearing in a developer's head, not in the engine.

3. **`Email LIKE '%@gmail.com'` → persisted reversed-host hash.** Added a `PERSISTED` computed column (`EmailHostReversedHash = CHECKSUM(REVERSE(SUBSTRING(Email, CHARINDEX('@', Email), 254)))`) and put it in the covering index. Equality on a precomputed hash is the SARGable form of an unanchored substring match.

4. **Scalar UDF inlined.** `dbo.fnFullName` becomes a `CONCAT` expression with `LTRIM`/`RTRIM`. The optimizer can now reason about it as a single Compute Scalar instead of a per-row user-defined call.

5. **`N'Smith'` → `'Smith'`.** Match the literal type to the column type. `LastName` is varchar; the N prefix forced a column-side implicit convert.

6. **OR chain → IN list.** `Status IN (1, 2, 3, 4, 5)` lets the cardinality estimator reason about the predicate as a single selectivity, and the optimizer rewrites it into a seek with multiple range subseeks when an index is present.

7. **Correlated `TOP 1` subquery in WHERE → `OUTER APPLY`.** Critical change. As a predicate, the correlated subquery is re-evaluated per row from the outer driver. As an `OUTER APPLY` it's invoked once per qualified customer (a few hundred rows after the new covering index), and the audit table needs a seek not a scan.

8. **`CAST(LoggedAt AS date) = '2026-05-01'` → half-open range `>= '2026-05-01' AND < '2026-05-02'`.** The CAST stripped the column type and forced a scan even on the audit index.

9. **`SET NOCOUNT ON; OPTION (RECOMPILE);`** added. NOCOUNT cleans up the per-statement `n rows affected` chatter. RECOMPILE here is targeted — the report's predicate values are stable per execution, and we'd rather take the compile cost than serve a bad cached plan.

## Indexes the rewrite creates

See `04_indexes.sql`. Net new:

- `IX_Customers_LastName_Status_HostHash` — covering for the report's outer driver. Key (LastName, Status, EmailHostReversedHash), includes (FirstName, Email, CreatedAt). Three-column key gives selectivity ordering matching the rewrite's WHERE.
- `IX_Orders_CustomerId_OrderDate_Inc` — supports the customer + 30-day range seek. Includes TotalCents and Status to avoid key lookups.
- `IX_OrderLines_OrderId_Inc` — nonclustered alt-key on OrderId for the join, INCLUDE columns so it's covering.
- `IX_AuditLog_Source_LoggedAt_Inc` — supports the `OUTER APPLY`. Index leaf nodes hold the Payload via INCLUDE.

## Expected behavior

Without the indexes, the baseline plan is dominated by:

- A clustered index **scan** of `Customers` (50k rows) because the WHERE is non-SARGable.
- For each of those rows, a **nested loop into Orders** that has no usable index — another scan, projected over the 30-day window.
- A **correlated TOP 1 scan of AuditLog** per outer row.
- A **Compute Scalar** evaluating `dbo.fnFullName` per row.

On a 2M-orders / 6M-lines dataset this is hours, not minutes.

With the indexes + rewrite, the plan should be:

- An **Index Seek** on `IX_Customers_LastName_Status_HostHash` — seeks to `LastName = 'Smith' AND Status IN (...) AND EmailHostReversedHash = ?`. A few hundred customers.
- A **Loop Join with Index Seek** on `IX_Orders_CustomerId_OrderDate_Inc`.
- An **Index Seek with Range** on `IX_AuditLog_Source_LoggedAt_Inc` via the OUTER APPLY.
- No Compute Scalar surprises, no Key Lookup, no Table Scan.

The conceptual speedup is 100× to 1,000× depending on the data shape. Run
`./05_run.sh baseline` then `./05_run.sh optimized` to capture actual times
on your container.

## Caveats

- The seed is deterministic-ish via `CHECKSUM(NEWID())` — your row counts will match but the data values will vary, so plan shapes are reproducible but exact wall-clock numbers will not be.
- The user is responsible for clearing the plan cache and the buffer pool fairly between runs. `05_run.sh` does both.
- Statistics may need updating after the seed completes; the run script does not invoke `UPDATE STATISTICS` to keep the comparison honest.
- The case study runs against `dbopt_case`. If you want to play in a non-isolated database, point the scripts elsewhere — but expect collisions.
