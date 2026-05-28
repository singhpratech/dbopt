SELECT c.CustomerId
FROM dbo.Customers c
WHERE c.CustomerId NOT IN (SELECT o.CustomerId FROM dbo.Orders o);