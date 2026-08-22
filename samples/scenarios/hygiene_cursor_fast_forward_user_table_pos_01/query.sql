-- Read-only forward-only cursor over a USER table with no DML in the loop:
-- still reported (downgraded to info), because the per-row PRINT/format work
-- usually has a single-SELECT equivalent.
DECLARE @name nvarchar(200);
DECLARE cur CURSOR LOCAL STATIC READ_ONLY FOR
    SELECT c.Name FROM dbo.Customers AS c;
OPEN cur;
FETCH NEXT FROM cur INTO @name;
WHILE @@FETCH_STATUS = 0
BEGIN
    PRINT @name;
    FETCH NEXT FROM cur INTO @name;
END
CLOSE cur;
DEALLOCATE cur;
