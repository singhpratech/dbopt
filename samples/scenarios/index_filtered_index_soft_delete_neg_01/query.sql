-- False-positive guard: a plain point-lookup on a non-soft-delete predicate.
-- The single WHERE predicate compares an id to an arbitrary literal (not = 0,
-- not = 'Y'/'N', not IS NULL), so it is not a filtered-index / soft-delete
-- pattern. index.filtered_index_opportunity_soft_delete must stay silent.
SELECT CustomerId, OrderTotal, PlacedAt
FROM dbo.Orders
WHERE CustomerId = 42;
