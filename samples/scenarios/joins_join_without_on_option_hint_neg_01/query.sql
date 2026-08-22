SELECT o.OrderId, c.Name
FROM dbo.Orders AS o
INNER JOIN dbo.Customers AS c ON c.CustomerId = o.CustomerId
OPTION (RECOMPILE, LOOP JOIN, HASH JOIN);
