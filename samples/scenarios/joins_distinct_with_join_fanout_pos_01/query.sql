SELECT DISTINCT c.CustomerId, c.Name
FROM dbo.Customers AS c
JOIN dbo.Orders AS o ON o.CustomerId = c.CustomerId;
