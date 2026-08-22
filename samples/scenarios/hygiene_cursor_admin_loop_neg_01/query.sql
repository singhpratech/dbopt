-- The documented exception: a read-only FAST_FORWARD cursor over the catalog,
-- driving a per-database admin action. There is no set-based rewrite for
-- "run this for each database", so hygiene.cursor must stay silent.
DECLARE @db sysname;
DECLARE db_cur CURSOR LOCAL FAST_FORWARD FOR
    SELECT d.name FROM sys.databases AS d WHERE d.state_desc = 'ONLINE' AND d.database_id > 4;
OPEN db_cur;
FETCH NEXT FROM db_cur INTO @db;
WHILE @@FETCH_STATUS = 0
BEGIN
    EXEC sys.sp_executesql N'USE ' + QUOTENAME(@db) + N'; DBCC CHECKDB WITH NO_INFOMSGS;';
    FETCH NEXT FROM db_cur INTO @db;
END
CLOSE db_cur;
DEALLOCATE db_cur;
