USE BlindTrial; SET NOCOUNT ON;
PRINT '=== index physical stats (frag / page density / forwarded) ===';
SELECT OBJECT_NAME(ps.object_id) tbl, ISNULL(i.name,'(heap)') idx, ps.index_type_desc, ps.page_count,
       CAST(ps.avg_fragmentation_in_percent AS decimal(5,1)) frag_pct, CAST(ps.avg_page_space_used_in_percent AS decimal(5,1)) page_full_pct,
       ps.forwarded_record_count, i.fill_factor
FROM sys.dm_db_index_physical_stats(DB_ID(), NULL, NULL, NULL, 'DETAILED') ps
LEFT JOIN sys.indexes i ON i.object_id = ps.object_id AND i.index_id = ps.index_id
WHERE ps.index_level = 0 AND ps.page_count > 0 AND OBJECT_NAME(ps.object_id) <> 'vNums'
ORDER BY tbl, idx;
PRINT '=== D16 stats staleness on Events ===';
SELECT s.name, sp.rows, sp.rows_sampled, sp.modification_counter, sp.last_updated, s.no_recompute
FROM sys.stats s CROSS APPLY sys.dm_db_stats_properties(s.object_id, s.stats_id) sp
WHERE s.object_id = OBJECT_ID('dbo.Events');
PRINT '=== D16 estimate vs actual for range query ===';
SET STATISTICS IO ON;
SELECT COUNT(*) FROM dbo.Events WHERE OccurredAt >= '2026-05-01';
SET STATISTICS IO OFF;
PRINT '=== D5 tables without PK ===';
SELECT t.name FROM sys.tables t WHERE NOT EXISTS (SELECT 1 FROM sys.key_constraints k WHERE k.parent_object_id = t.object_id AND k.type='PK') AND t.name <> 'vNums';
PRINT '=== D2/B3 index key/include map ===';
SELECT OBJECT_NAME(i.object_id) tbl, i.name, STRING_AGG(CASE WHEN ic.is_included_column=0 THEN c.name END, ',') WITHIN GROUP (ORDER BY ic.key_ordinal) keys,
       STRING_AGG(CASE WHEN ic.is_included_column=1 THEN c.name END, ',') incl
FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
WHERE OBJECT_NAME(i.object_id) IN ('Customers','Products') GROUP BY OBJECT_NAME(i.object_id), i.name ORDER BY 1,2;
PRINT '=== D18 deprecated column types ===';
SELECT OBJECT_NAME(c.object_id) tbl, c.name, TYPE_NAME(c.user_type_id) typ FROM sys.columns c WHERE TYPE_NAME(c.user_type_id) IN ('text','ntext','image');
PRINT '=== D16/D13 estimate vs actual (STATISTICS PROFILE) ===';
SET STATISTICS PROFILE ON;
SELECT COUNT(*) FROM dbo.Events WHERE OccurredAt >= '2026-05-01';
SET STATISTICS PROFILE OFF;
