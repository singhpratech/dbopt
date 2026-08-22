-- WITH RECOMPILE in the proc header is not OPTION (RECOMPILE); and the only
-- OPTION (RECOMPILE) below sits on a table-variable statement with no
-- `col = @param` predicate.
CREATE PROCEDURE dbo.BackupAll
    @Databases nvarchar(max)
WITH RECOMPILE
AS
BEGIN
    DECLARE @tmpDatabases TABLE (ID int, DatabaseNameFS nvarchar(128), Selected bit);
    UPDATE @tmpDatabases SET Selected = 0
    WHERE UPPER(DatabaseNameFS) IN (SELECT UPPER(DatabaseNameFS) FROM @tmpDatabases GROUP BY UPPER(DatabaseNameFS) HAVING COUNT(*) > 1)
    OPTION (RECOMPILE);
END;
