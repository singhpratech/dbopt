-- The function wraps a local VARIABLE; the column db.name stays bare and seekable.
-- The <> predicate and the CASE in the next SELECT list are not join keys either.
SELECT DISTINCT db.database_id
FROM a
INNER JOIN sys.databases AS db ON LTRIM(RTRIM(SUBSTRING(@FilterPlansByDatabase, a.f, a.t - a.f))) = db.name
SELECT CASE WHEN DATEDIFF(HOUR, ISNULL(der.start_time, bi.last_batch), SYSDATETIME()) > 576 THEN 1 ELSE 0 END
FROM sys.dm_exec_requests AS der
INNER JOIN #ia AS ia1 ON ia1.object_id = der.session_id
INNER JOIN #ia AS ia2 ON ia2.object_id = ia1.object_id AND ISNULL(ia1.included_columns, '') <> ISNULL(ia2.included_columns, '')
