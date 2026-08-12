-- The arithmetic moved to the constant side, so the column stays bare and
-- the index on PlacedAt can still be seeked.
SELECT OrderId FROM dbo.Orders WHERE PlacedAt < DATEADD(day, -30, SYSUTCDATETIME());
