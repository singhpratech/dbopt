-- A cursor variable declaration plus a catalog-driven per-database loop that
-- collects results into a temp table: the documented DBA-loop exemption.
DECLARE @db sysname, @sql nvarchar(max);
DECLARE @c CURSOR;
SET @c = CURSOR LOCAL FAST_FORWARD FOR
    SELECT d.name FROM sys.databases AS d WHERE d.state_desc = 'ONLINE';
OPEN @c;
FETCH NEXT FROM @c INTO @db;
WHILE @@FETCH_STATUS = 0
BEGIN
    SET @sql = N'USE ' + QUOTENAME(@db) + N'; SELECT DB_NAME(), COUNT(*) FROM sys.tables;';
    INSERT INTO #results (db, table_count) EXEC sp_executesql @sql;
    FETCH NEXT FROM @c INTO @db;
END
CLOSE @c;
DEALLOCATE @c;
