-- Dynamic SQL assembled into a variable and run with EXEC(@sql). This is
-- unparameterized: every distinct value of @schema/@table compiles a fresh plan
-- and the string is wide open to SQL injection.
DECLARE @sql nvarchar(max);
SET @sql = N'SELECT COUNT(*) FROM ' + QUOTENAME(@schema) + N'.' + QUOTENAME(@table);
EXEC(@sql);
