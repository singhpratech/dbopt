-- A schema-qualified scalar UDF in a JOIN ON and in a HAVING is still a
-- predicate after the scoping change.
SELECT o.CustomerId, COUNT(*) AS n
FROM dbo.Orders AS o
JOIN dbo.Customers AS c ON c.Id = dbo.fn_ResolveCustomer(o.CustomerRef)
GROUP BY o.CustomerId
HAVING dbo.fn_Tier(o.CustomerId) > 1;
