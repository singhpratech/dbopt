DECLARE @body text;
SELECT CAST(st.text AS text) AS sql_text
FROM sys.dm_exec_sql_text(0x00) AS st;
