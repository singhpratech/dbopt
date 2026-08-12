-- `INSTEAD OF DELETE` is a trigger event, not a DELETE statement.
CREATE TRIGGER dbo.trg_NoDelete ON dbo.Orders
INSTEAD OF DELETE AS
BEGIN
    SET NOCOUNT ON;
    RAISERROR ('Deletes are not permitted.', 16, 1);
END
