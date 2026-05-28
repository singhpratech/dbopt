-- Hot query that only ever looks at the live (non-deleted) rows. A filtered
-- nonclustered index WHERE IsDeleted = 0 would be smaller, carry accurate
-- filtered statistics, and avoid indexing the cold archived rows.
SELECT CustomerId, OrderTotal, PlacedAt
FROM dbo.Orders
WHERE IsDeleted = 0
ORDER BY PlacedAt DESC;
