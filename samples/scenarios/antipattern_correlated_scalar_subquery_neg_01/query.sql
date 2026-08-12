-- Rewritten as a join: evaluated once as a set, not once per outer row.
SELECT c.CustomerId, x.LastOrder
FROM dbo.Customers AS c
LEFT JOIN (SELECT CustomerId, MAX(PlacedAt) AS LastOrder FROM dbo.Orders GROUP BY CustomerId) AS x
       ON x.CustomerId = c.CustomerId;
