-- Bounded by TOP, and the target is a temp table. Reading the target as `TOP`
-- made every one of those checks miss.
DELETE TOP (5000) FROM #t;
DELETE TOP (5000) FROM dbo.EventLog;
