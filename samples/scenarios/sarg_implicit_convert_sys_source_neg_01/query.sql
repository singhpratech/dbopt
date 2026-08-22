-- Catalog views and DMVs are nvarchar throughout. `sys.objects.name = N'…'`
-- is the correct literal, and there is no user index to design for.
SELECT o.object_id
FROM sys.objects AS o
JOIN sys.schemas AS s ON s.schema_id = o.schema_id
WHERE o.name = N'Orders' AND s.name = N'dbo';
SELECT database_id FROM sys.databases WHERE name = N'master';
SELECT session_id FROM sys.dm_exec_sessions WHERE program_name = N'dbopt';
