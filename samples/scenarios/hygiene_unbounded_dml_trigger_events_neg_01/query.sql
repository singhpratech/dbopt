-- A trigger's event list names the events it fires on; `IF UPDATE(col)` is the
-- trigger function that tests whether a column took part in the statement.
CREATE TRIGGER dbo.trg_Audit ON dbo.Orders
AFTER INSERT, UPDATE, DELETE AS
BEGIN
    SET NOCOUNT ON;
    IF UPDATE(Status) OR UPDATE(Total)
        INSERT INTO dbo.Audit (Note) VALUES (N'changed');
END
