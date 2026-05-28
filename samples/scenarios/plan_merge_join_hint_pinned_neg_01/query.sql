-- False-positive guard: a plain INNER JOIN with no physical-algorithm hint.
-- The optimizer is free to choose loop/hash/merge and to use adaptive joins.
-- There is no LOOP/MERGE/HASH/REMOTE join hint and no OPTION (... JOIN), so
-- plan.merge_join_hint_pinned must stay silent.
SELECT o.OrderId, c.CustomerName
FROM dbo.Orders AS o
INNER JOIN dbo.Customers AS c ON c.CustomerId = o.CustomerId
WHERE o.PlacedAt >= '2026-01-01';
