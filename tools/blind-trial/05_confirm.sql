USE BlindTrial; SET NOCOUNT ON;
PRINT '=== missing index details (BlindTrial) ===';
SELECT OBJECT_NAME(d.object_id) tbl, d.equality_columns eq, d.inequality_columns ineq, d.included_columns incl, s.user_seeks seeks, CAST(s.avg_user_impact AS int) impact, s.avg_total_user_cost cost
FROM sys.dm_db_missing_index_details d JOIN sys.dm_db_missing_index_groups g ON g.index_handle=d.index_handle JOIN sys.dm_db_missing_index_group_stats s ON s.group_handle=g.index_group_handle
WHERE d.database_id = DB_ID() ORDER BY s.user_seeks DESC;
PRINT '=== index usage stats (BlindTrial, all indexes incl. untouched) ===';
SELECT OBJECT_NAME(i.object_id) tbl, ISNULL(i.name,'(heap)') idx, ISNULL(u.user_seeks,0) seeks, ISNULL(u.user_scans,0) scans, ISNULL(u.user_lookups,0) lookups, ISNULL(u.user_updates,0) updates
FROM sys.indexes i JOIN sys.tables t ON t.object_id=i.object_id LEFT JOIN sys.dm_db_index_usage_stats u ON u.database_id=DB_ID() AND u.object_id=i.object_id AND u.index_id=i.index_id
WHERE t.name <> 'vNums' ORDER BY tbl, idx;
PRINT '=== heap after workload ===';
SELECT OBJECT_NAME(object_id) tbl, page_count, forwarded_record_count, record_count FROM sys.dm_db_index_physical_stats(DB_ID(), OBJECT_ID('dbo.AuditLog'), 0, NULL, 'DETAILED');
PRINT '=== Query Store: top procs by total duration ===';
SELECT TOP 25 OBJECT_NAME(q.object_id) proc_name, LEFT(REPLACE(REPLACE(qt.query_sql_text,CHAR(10),' '),CHAR(13),' '),70) sql_text, SUM(rs.count_executions) execs,
  CAST(SUM(rs.avg_duration*rs.count_executions)/1000 AS bigint) total_ms, CAST(SUM(rs.avg_duration*rs.count_executions)/SUM(rs.count_executions)/1000 AS int) avg_ms,
  CAST(MAX(rs.max_duration)/1000 AS int) max_ms, CAST(SUM(rs.avg_logical_io_reads*rs.count_executions)/SUM(rs.count_executions) AS bigint) avg_reads, CAST(MAX(rs.max_logical_io_reads) AS bigint) max_reads,
  COUNT(DISTINCT p.plan_id) plans
FROM sys.query_store_query q JOIN sys.query_store_query_text qt ON qt.query_text_id=q.query_text_id JOIN sys.query_store_plan p ON p.query_id=q.query_id JOIN sys.query_store_runtime_stats rs ON rs.plan_id=p.plan_id
WHERE q.object_id <> 0 GROUP BY q.object_id, qt.query_sql_text ORDER BY total_ms DESC;
PRINT '=== QS memory grant / spill hint for EventDump ===';
SELECT OBJECT_NAME(q.object_id) proc_name, MAX(rs.max_query_max_used_memory)*8 max_grant_kb, MAX(rs.max_tempdb_space_used)*8 max_tempdb_kb, SUM(rs.count_executions) execs
FROM sys.query_store_query q JOIN sys.query_store_plan p ON p.query_id=q.query_id JOIN sys.query_store_runtime_stats rs ON rs.plan_id=p.plan_id
WHERE q.object_id = OBJECT_ID('dbo.usp_EventDump') GROUP BY q.object_id;
PRINT '=== D13/D16 sniffed estimate inside proc ===';
SELECT CAST(p.query_plan AS nvarchar(max)) plan_xml FROM sys.query_store_query q JOIN sys.query_store_plan p ON p.query_id=q.query_id WHERE q.object_id = OBJECT_ID('dbo.usp_EventDump');
PRINT '=== QS options ===';
SELECT actual_state_desc, query_capture_mode_desc FROM sys.database_query_store_options;
