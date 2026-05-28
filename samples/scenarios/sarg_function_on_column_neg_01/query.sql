-- Functions appear, but on literals, not on indexed columns.
SELECT OrderId, OrderDate
FROM dbo.Orders
WHERE OrderDate >= DATEADD(day, -7, GETDATE())
  AND OrderDate <  GETDATE();
