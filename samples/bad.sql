-- Deliberately bad sample: each block triggers a different rule
CREATE PROCEDURE GetCustomers
AS
BEGIN
    SELECT *
    FROM Customers WITH (NOLOCK)
    WHERE UPPER(LastName) = 'SMITH'
      AND Email LIKE '%@example.com'
      AND dbo.fnFullName(FirstName, LastName) = N'John Smith'
      AND CreatedAt >= '2025-01-01'
      OR  Status = 1
      OR  Status = 2
      OR  Status = 3
      OR  Status = 4;

    DECLARE @c CURSOR;

    SELECT TOP 10 OrderId FROM Orders;

    UPDATE Customers SET LastSeen = GETDATE();

    DECLARE @sql NVARCHAR(MAX) = 'SELECT * FROM Orders WHERE Id = ' + CAST(@id AS NVARCHAR(10));
    EXEC (@sql);
END
