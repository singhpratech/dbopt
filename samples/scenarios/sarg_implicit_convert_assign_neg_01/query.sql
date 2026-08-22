-- Every `= N'…'` here is an assignment, a default or a named argument. No
-- column is compared, so nothing can be implicitly converted per row.
CREATE PROCEDURE dbo.usp_Demo @username sysname = N'', @mode nvarchar(10) = N'fast'
AS
BEGIN
    DECLARE @sql nvarchar(max) = N'SELECT 1';
    SET @sql = N'USE ' + QUOTENAME(@db) + N'; SELECT 1;';
    SELECT @sql = N'IF EXISTS (SELECT 1) PRINT 1;';
    UPDATE ia1 SET ia1.consolidation_rule = N'Same Keys Different Order'
    FROM #index_analysis AS ia1 WHERE ia1.index_id > 1;
    SELECT script_type = N'MERGE SCRIPT', ia1.index_name
    FROM #index_analysis AS ia1;
    EXEC sp_executesql @sql, @params = N'@i_DatabaseName nvarchar(128)', @i_DatabaseName = @db;
    EXECUTE dbo.usp_Other @DatabaseContext = N'master';
END;
