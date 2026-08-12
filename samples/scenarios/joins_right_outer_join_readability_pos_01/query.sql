SELECT o.OrderId, c.Name
FROM dbo.Orders AS o
RIGHT OUTER JOIN dbo.Customers AS c ON c.CustomerId = o.CustomerId;
