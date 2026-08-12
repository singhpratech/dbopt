-- The TOP bounds a *joined* subquery, not the update target: every row of
-- dbo.T is still rewritten.
UPDATE t SET x = 1
FROM dbo.T t
CROSS JOIN (SELECT TOP 1 n FROM dbo.U) q;
