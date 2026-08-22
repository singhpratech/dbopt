-- `text` here is a COLUMN of sys.dm_exec_sql_text, not the deprecated type.
SELECT r.session_id, st.text AS sql_text, [text]
FROM sys.dm_exec_requests AS r
CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) AS st
WHERE st.text LIKE '%dbo.Orders%'
ORDER BY text;
