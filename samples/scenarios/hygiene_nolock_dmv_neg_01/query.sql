-- NOLOCK on a DMV / catalog view is inert: there are no data pages to dirty-read.
SELECT TOP 1 qs.creation_time
FROM sys.dm_exec_query_stats AS qs WITH (NOLOCK)
ORDER BY qs.creation_time;

SELECT t.name FROM sys.tables WITH (NOLOCK) AS t;
