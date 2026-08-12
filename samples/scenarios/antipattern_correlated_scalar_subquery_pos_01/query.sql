SELECT c.CustomerId,
       (SELECT MAX(o.PlacedAt) FROM dbo.Orders o WHERE o.CustomerId = c.CustomerId) AS LastOrder
FROM dbo.Customers AS c;
