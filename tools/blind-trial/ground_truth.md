# BlindTrial — ground truth (built 2026-08-22, SQL Server 2025 CU4, container `pharma-db`, db `BlindTrial`)

Built independently of dbopt (no dbopt source or rules consulted). Rebuild: `source env.sh; sqm -i 01_schema_data.sql; sq -i 02_indexes_procs.sql; ./run_workload.sh 240` (total build ~15 s; workload 4 min).
Evidence columns are measured on this instance with `SET STATISTICS IO/TIME`, `sys.dm_db_index_physical_stats`, `sys.dm_db_stats_properties`, and Query Store after 6 min of workload (4,382 + 2,221 iterations, 0 errors). "After" numbers come from the fix applied inside a rolled-back transaction (`03_measure.sql`).

Tables: Customers 200k, Products 50k, Categories 20 (heap), Orders 1M, OrderLines 2M, Shipments 300k, Events 1M, AuditLog ~106k (heap), PriceChanges (cursor queue).

## Planted defects (18)

| # | Category | Object(s) | Defect | Ideal fix | Measured evidence (before → after) |
|---|----------|-----------|--------|-----------|------------------------------------|
| D1 | Missing index (real repeated query) | `dbo.Orders`, `usp_OrdersByChannelDate` | `WHERE Channel=@c AND OrderDate>=@f AND OrderDate<@t` has no supporting index → clustered scan of 1M rows | `CREATE INDEX ... ON Orders(Channel, OrderDate) INCLUDE (CustomerID, TotalAmount)` | 6,452 logical reads, 254 ms CPU → **26 reads**. DMV: `missing_index_details` Orders eq=[Channel] ineq=[OrderDate] incl=[CustomerID],[TotalAmount], 2,062 seeks, impact 78. QS: 2,062 execs, avg 6,447 reads |
| D2 | Duplicate index | `Customers.IX_Customers_Email` / `IX_Customers_Email_Dup` | Identical key (Email), no includes, both nonclustered | `DROP INDEX IX_Customers_Email_Dup` | Same key/include in `sys.index_columns`; 910 pages each (7 MB wasted); `_Dup` usage 0 seeks/0 scans/0 lookups/0 updates after workload |
| D3 | Unused index | `Customers.IX_Customers_Phone` | Never referenced by any query; no writes to Customers | `DROP INDEX IX_Customers_Phone` | 648 pages; `dm_db_index_usage_stats` 0/0/0/0 after 6 min workload (all other Customers indexes have activity) |
| D4 | Heap with real writes | `dbo.AuditLog` (nonclustered PK only) | Heap receives inserts + in-place row growth → forwarded records, RID lookups | `CREATE CLUSTERED INDEX ON AuditLog(AuditID)` (or make PK clustered) | heap: 764 pages, **1,313 forwarded_record_count** (0 before workload), 5,915+ user_updates; `PK_AuditLog` 2,957 seeks each followed by RID lookup |
| D5 | No primary key | `dbo.Shipments` | Non-unique clustered index `CIX_Shipments(ShipmentID)`, no PK / unique constraint (300k rows) | `ALTER TABLE Shipments ADD CONSTRAINT PK_Shipments PRIMARY KEY CLUSTERED (ShipmentID)` (drop CIX first) | `sys.key_constraints` has no PK row; CIX non-unique → 4-byte uniquifier overhead |
| D6 | Wide/random clustered key | `dbo.Events` `PK_Events(EventGuid uniqueidentifier DEFAULT NEWID())` | 16-byte random GUID clustered key; carried into both NC indexes; random inserts fragment | Clustered on `(OccurredAt, EventID)` / sequential key; keep GUID as NC unique | PK_Events: 34,346 pages, **93.4% fragmentation, 62.1% page density**; IX_Events_CustomerID 99.2% frag / 62.3% density; IX_Events_OccurredAt 99.2% frag |
| D7 | Non-sargable: function on column | `usp_CustomerByEmail`: `WHERE LOWER(Email) = LOWER(@Email)` | Index on Email can't be seeked | `WHERE Email = @Email` (collation is CI anyway) | 922 reads / 17 ms (index scan) → **3 reads**. QS: 2,958 execs @ 922 reads |
| D8 | Non-sargable: leading wildcard | `usp_ProductSearch`: `Name LIKE '%'+@Term+'%'` | Full scan of Products every call | Full-text index / trailing-wildcard only / n-gram table | 425 reads, 28 ms CPU per call; QS 2,064 execs. DMV suggests index on Products ineq=[Name] (which would NOT actually help — a tool that just parrots the DMV here is wrong) |
| D9 | Implicit conversion | `usp_ShipmentByTracking @Code nvarchar(40)` vs `Shipments.TrackingCode varchar(40)` | nvarchar param → CONVERT_IMPLICIT on column → index scan + residual | Declare `@Code varchar(40)` | 1,134 reads / 19 ms vs **3 reads** for the correctly-typed twin `usp_ShipmentByTrackingTyped`; QS 2,958 execs each |
| D10 | Scalar UDF per row | `dbo.fn_CustomerTierLabel` (data-accessing scalar UDF) in `usp_OrdersWithTier` | UDF executed ~50k times/call; hidden Customers reads | Inline as JOIN + CASE, or inline TVF | Customers **150,000 reads** + 336 Orders reads, 123 ms CPU → inline: 1,778 + 321 reads, 36 ms. QS max 150,336 reads |
| D11 | Parameter sniffing | `usp_OrdersByStatus @Status` + key-only `IX_Orders_Status`; Status skew CANCELLED 1,000 / PENDING 60k / RETURNED 19k / SHIPPED 920k | Plan compiled for CANCELLED (seek + 1,000 lookups) reused for SHIPPED | `OPTION (RECOMPILE)` / covering index `(Status) INCLUDE (CustomerID, OrderDate, TotalAmount)` / OPTIMIZE FOR | CANCELLED: 3,077 reads. SHIPPED on CANCELLED's plan: **2,819,900 reads / 674 ms** vs 6,452 reads on its own plan. QS: 2,167 execs, avg 196,558 reads, max 2,934,927, 2 plans. DMV also lists Orders eq=[Status] incl=[CustomerID],[OrderDate],[TotalAmount] (2,163 seeks) |
| D12 | Missing FK-supporting index | `FK_Orders_Customers` on `Orders.CustomerID` — no index | Every customer→orders join scans 1M-row Orders | `CREATE INDEX ON Orders(CustomerID) INCLUDE (TotalAmount)` | `usp_CustomerOrderSummary`: 6,452 reads → **3 reads**. QS: 4,444 execs, avg 6,453 reads, #2 by total duration (117 s). DMV: Orders eq=[CustomerID], 1,485 seeks, impact 99 |
| D13 | `SELECT *` + ORDER BY on large set | `usp_EventDump`: `SELECT * FROM Events WHERE OccurredAt>=@Since ORDER BY Payload DESC` | Pulls 50k–150k wide rows incl. 400-char Payload; scans clustered (1M rows read); sort needs 126 MB grant | Project needed columns; index supports the range; don't sort on Payload | 35,222 reads / 567 ms per call; plan: Clustered Index Scan EstimatedRowsRead=1e6, Sort; QS max memory grant 128,928 KB; 99 execs |
| D14 | NOLOCK | `usp_DashboardTotals`: `WITH (NOLOCK)` on Orders and OrderLines | Dirty reads / double-counted revenue on a financial aggregate | Remove hint; use RCSI if blocking is the concern | Static (hint present in proc text); 1,361+ execs in QS |
| D15 | Cursor doing row-by-row DML | `usp_ApplyPriceChanges`: cursor over PriceChanges, 2 UPDATEs per row | RBAR; 3 QS entries (cursor + 2 per-row UPDATEs, ~8,900 execs each) | Single set-based `UPDATE ... FROM Products JOIN PriceChanges` | 2,000 rows: cursor **2,441 ms** vs set-based **20 ms** (same rows, rolled back) |
| D16 | Stale statistics | `Events.IX_Events_OccurredAt` created `WITH (STATISTICS_NORECOMPUTE = ON)`; 600k rows loaded after stats built, spread over a date range the histogram never saw | Stats rows=400,000 while table has 1,000,000; never auto-updates | `UPDATE STATISTICS Events IX_Events_OccurredAt WITH FULLSCAN` and remove NORECOMPUTE | `dm_db_stats_properties`: rows 400,000, modification_counter **600,000**, no_recompute=1; rows >= 2026-05-01 actual 150,563 |
| D17 | Low fill factor / wasted space | `OrderLines.IX_OrderLines_ProductID` `FILLFACTOR = 50` on a read-only (in workload) table | Index twice the size it needs to be | Rebuild with FILLFACTOR 100 (or 90) | 12,354 pages at **50.0% avg_page_space_used** (vs ~6,200 full); 2,957 seeks, 0 updates |
| D18 | Deprecated features | `Products.Description ntext`, `Shipments.Notes text`; `usp_TopProductsInCategory` uses `SET ROWCOUNT 10` | Deprecated LOB types; ROWCOUNT is deprecated for DML and not plan-aware | `nvarchar(max)` / `varchar(max)`; `SELECT TOP (10)` | `sys.columns` type_name text/ntext; proc text contains `SET ROWCOUNT 10` (2,958 execs) |

Also true (secondary, don't double-count): D6's Events GUID key is also the fragmentation example; D11's DMV entry and D1/D12 mean the missing-index DMV holds 5 rows for Orders/Products.

## Must-NOT-flag list (benign look-alikes, 5)

| # | Object | Why it looks bad | Why it is fine / what a correct tool says |
|---|--------|------------------|--------------------------------------------|
| B1 | `usp_DatabaseInventory` | Declares a CURSOR + WHILE loop | Catalog-only `FAST_FORWARD` cursor over `sys.databases` (6 rows), no DML, 0 ms. Must not be flagged as row-by-row DML / performance cursor |
| B2 | `dbo.Categories` heap | Heap, no PK, table-scanned 2,958 times | 20 rows, 1 page, 1 write ever, 1 logical read per lookup. A heap/missing-index/no-PK finding here must be low-severity or absent; a "critical heap" call is a false positive |
| B3 | `Products.IX_Products_CategoryID_Price` vs `IX_Products_CategoryID_Stock` | Same key column (CategoryID) → looks like a duplicate | Different INCLUDE sets, each covering its own proc (`usp_CategoryPriceStats` 5,915 seeks; `usp_CategoryLowStock` 2,957 seeks). Must NOT be reported as an exact duplicate or "drop one". (Suggesting a merged index `(CategoryID) INCLUDE (Price, StockQty, SKU)` is acceptable, low priority) |
| B4 | `usp_CategoryByName` `WHERE Name=@Name` on Categories | Unindexed predicate → table scan | 1-page table, 1 logical read. Must not produce a missing-index recommendation |
| B5 | `usp_ShipmentByTrackingTyped @Code varchar(40)` | Twin of D9 | Correctly typed: 3 reads, seek. Must not be flagged for implicit conversion (a tool flagging both twins is pattern-matching on the proc name/shape, not the types) |

Extra negatives: `IX_OrderLines_OrderID` (4,318 seeks) and `IX_Shipments_TrackingCode` are healthy and used; `PK_Orders`/`PK_OrderLines`/`PK_Customers` have 0% fragmentation and ~99.6% density — no rebuild recommendation is warranted on them.

## Workload confirmation (after 240 s + 120 s runs)
- Query Store: READ_WRITE, capture ALL, 1-min intervals; 23 distinct proc statements captured.
- `sys.dm_db_missing_index_details` for BlindTrial: 5 rows (Orders ×4, Products ×1).
- `sys.dm_db_index_usage_stats`: every index listed; `IX_Customers_Phone` and `IX_Customers_Email_Dup` are the only two at 0/0/0/0.
- AuditLog heap: 1,313 forwarded records, 764 pages.
- Top by total duration: usp_OrdersByStatus 121.8 s, usp_CustomerOrderSummary 117.2 s, usp_CustomerByEmail, usp_ShipmentByTracking, usp_OrdersByChannelDate.

## Caveats
- Everything is in-memory on a 1-socket dev box: durations are small in absolute terms; logical reads are the stable metric.
- The missing-index DMV entry for D8 (Products ineq=[Name]) is a red herring from SQL Server itself — an index on Name cannot serve `LIKE '%x%'`.
- The `_Dup` index shows as "unused" too: crediting D2 requires the tool to call it duplicate/redundant, but "unused, drop it" is a partial credit.
- D16's estimate inside the proc is 180,000 (density fallback) vs actuals 50k–150k, so the estimation gap is moderate; the primary evidence is `modification_counter`/`no_recompute`.
- DMV counters reset on instance restart; re-run `run_workload.sh` before grading if `pharma-db` has restarted.
