SELECT c.CustomerID, o.OrderID
FROM dbo.Customers AS c
LEFT JOIN dbo.Orders AS o ON o.CustomerID = c.CustomerID
WHERE COALESCE(o.Freight, 0) > 10
  AND o.OrderDate >= '2024-01-01';
