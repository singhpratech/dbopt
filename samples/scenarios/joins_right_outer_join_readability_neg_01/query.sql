-- The same query expressed left-to-right.
SELECT o.OrderId, c.Name
FROM dbo.Customers AS c
LEFT OUTER JOIN dbo.Orders AS o ON o.CustomerId = c.CustomerId;
