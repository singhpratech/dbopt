-- ANSI join syntax: the relationship is in the ON clause where it belongs.
SELECT o.OrderId, c.Name
FROM dbo.Orders AS o
INNER JOIN dbo.Customers AS c ON c.CustomerId = o.CustomerId;
