-- Row-by-row INSERT per fetched row over a user table: the set-based
-- INSERT ... SELECT rewrite exists, so the rule must fire.
DECLARE @id int, @total money;
DECLARE cur CURSOR LOCAL FAST_FORWARD FOR
    SELECT o.OrderId, o.Total FROM dbo.Orders AS o WHERE o.Archived = 0;
OPEN cur;
FETCH NEXT FROM cur INTO @id, @total;
WHILE @@FETCH_STATUS = 0
BEGIN
    INSERT INTO dbo.OrderArchive (OrderId, Total, ArchivedAt) VALUES (@id, @total, SYSDATETIME());
    FETCH NEXT FROM cur INTO @id, @total;
END
CLOSE cur;
DEALLOCATE cur;
