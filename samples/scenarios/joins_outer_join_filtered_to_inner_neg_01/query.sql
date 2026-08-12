-- The predicate is in the ON clause, so unmatched rows are still preserved.
SELECT c.CustomerId, o.OrderId
FROM dbo.Customers AS c
LEFT JOIN dbo.Orders AS o ON o.CustomerId = c.CustomerId AND o.Status = 'shipped';
