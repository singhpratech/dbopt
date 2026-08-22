-- The loop body builds dynamic SQL: the only INSERT/DELETE words sit inside
-- string literals, a RAISERROR message and a comment, never as statements.
DECLARE @area_name sysname, @temp_table sysname, @insert_list nvarchar(max), @sql nvarchar(max);
DECLARE @collection_cursor CURSOR;
SET @collection_cursor = CURSOR LOCAL SCROLL DYNAMIC READ_ONLY FOR
    SELECT ca.area_name, ca.temp_table, ca.insert_list
    FROM @collection_areas AS ca
    WHERE ca.should_collect = 1;
OPEN @collection_cursor;
FETCH NEXT FROM @collection_cursor INTO @area_name, @temp_table, @insert_list;
WHILE @@FETCH_STATUS = 0
BEGIN
    -- INSERT the parsed rows for this area (done by the dynamic statement)
    RAISERROR(N'Processing %s: INSERT into %s', 0, 1, @area_name, @temp_table) WITH NOWAIT;
    SET @sql = N'INSERT INTO ' + QUOTENAME(@temp_table) + N' (' + @insert_list + N') SELECT * FROM #xml_rows; DELETE #xml_rows;';
    EXECUTE sys.sp_executesql @sql;
    FETCH NEXT FROM @collection_cursor INTO @area_name, @temp_table, @insert_list;
END;
