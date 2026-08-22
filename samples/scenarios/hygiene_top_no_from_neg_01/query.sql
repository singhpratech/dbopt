-- TOP on a single-row assignment with no row source, and TOP 1 over a DMF
-- that returns one row per handle: neither can pick an arbitrary subset.
SELECT TOP (1)
    @OnlyQueryHashes = STUFF((SELECT DISTINCT N',' + CONVERT(NVARCHAR(MAX), qhg.query_hash, 1)
                              FROM #query_hash_grouped AS qhg
                              WHERE qhg.query_hash <> 0x00
                              FOR XML PATH(N''), TYPE).value(N'.[1]', N'NVARCHAR(MAX)'), 1, 1, N'')
OPTION (RECOMPILE);

SELECT s.spid,
       (SELECT TOP 1 [text] FROM sys.dm_exec_sql_text(c.most_recent_sql_handle)) AS QueryText
FROM sys.sysprocesses AS s
INNER JOIN sys.dm_exec_connections AS c ON s.spid = c.session_id
WHERE s.open_tran > 0;
