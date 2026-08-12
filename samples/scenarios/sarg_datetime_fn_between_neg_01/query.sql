-- The same rows as a half-open range on the bare column: seekable.
SELECT OrderId FROM dbo.Orders
WHERE PlacedAt >= '2024-01-01' AND PlacedAt < '2026-01-01';
