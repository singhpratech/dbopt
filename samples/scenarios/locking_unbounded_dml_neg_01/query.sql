-- False-positive guard: a batched DELETE that already has BOTH a WHERE clause
-- and TOP (n) batching. This is the textbook-correct chunked-delete shape, so
-- neither hygiene.unbounded_dml (missing WHERE) nor locking.update_without_index
-- (WHERE but no TOP) should fire.
DELETE TOP (1000) FROM dbo.AuditLog
WHERE CreatedUtc < DATEADD(DAY, -90, SYSUTCDATETIME());

-- An UPDATE that is likewise both filtered and TOP-batched.
UPDATE TOP (500) dbo.Orders
SET Status = 9
WHERE Status = 3 AND OrderDate < '2024-01-01';
