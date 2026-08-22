-- Different sources AND a distinguishing literal per branch: the rows are
-- provably disjoint, so the implicit DISTINCT is pure waste.
SELECT City, CompanyName, ContactName, 'Customers' AS Relationship
FROM dbo.Customers
UNION
SELECT City, CompanyName, ContactName, 'Suppliers'
FROM dbo.Suppliers;
