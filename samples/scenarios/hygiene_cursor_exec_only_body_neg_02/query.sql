-- A cursor over a prepared command list whose loop only EXECs each row: the
-- same per-row admin loop as one over sys.databases, with no set-based form.
DECLARE @drop_cursor CURSOR;
DECLARE @drop_old_sql nvarchar(max);
SET @drop_cursor = CURSOR LOCAL SCROLL DYNAMIC READ_ONLY FOR
    SELECT drop_command FROM #drop_commands;
OPEN @drop_cursor;
FETCH NEXT FROM @drop_cursor INTO @drop_old_sql;
WHILE @@FETCH_STATUS = 0
BEGIN
    PRINT @drop_old_sql;
    EXECUTE (@drop_old_sql);
    FETCH NEXT FROM @drop_cursor INTO @drop_old_sql;
END;

/* unrelated work later in the same batch must not be blamed on the cursor */
INSERT INTO #results (name) SELECT name FROM sys.databases;
