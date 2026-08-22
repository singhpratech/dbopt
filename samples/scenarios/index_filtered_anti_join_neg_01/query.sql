-- `LEFT JOIN ... WHERE o.OrderId IS NULL` is the anti-join idiom: the NULL is
-- produced by the outer join, not stored in a nullable column, so a filtered
-- index `WHERE OrderId IS NULL` would index nothing.
SELECT c.CustomerId, c.Name
FROM dbo.Customers AS c
LEFT JOIN dbo.Orders AS o
    ON o.CustomerId = c.CustomerId
WHERE o.OrderId IS NULL;
