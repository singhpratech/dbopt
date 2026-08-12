SELECT o.OrderId, c.Name
FROM dbo.Orders AS o, dbo.Customers AS c
WHERE c.CustomerId = o.CustomerId;
