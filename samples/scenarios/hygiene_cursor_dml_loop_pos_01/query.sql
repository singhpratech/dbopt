-- Row-by-row DML over a user table: the case the rule exists for, and it is
-- still a warning even though the cursor is declared FAST_FORWARD.
DECLARE @id int;
DECLARE cur CURSOR LOCAL FAST_FORWARD FOR
    SELECT o.OrderId FROM dbo.Orders AS o WHERE o.Status = 0;
OPEN cur;
FETCH NEXT FROM cur INTO @id;
WHILE @@FETCH_STATUS = 0
BEGIN
    UPDATE dbo.Orders SET Status = 1, UpdatedAt = SYSDATETIME() WHERE OrderId = @id;
    FETCH NEXT FROM cur INTO @id;
END
CLOSE cur;
DEALLOCATE cur;
