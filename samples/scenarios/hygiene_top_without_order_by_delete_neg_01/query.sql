-- The chunked-delete loop that locking.dml_without_batching itself recommends.
-- ORDER BY is not legal on DELETE, so demanding one is advice nobody can take.
WHILE 1 = 1
BEGIN
    DELETE TOP (5000) FROM dbo.EventLog WHERE CreatedUtc < DATEADD(day, -90, SYSUTCDATETIME());
    IF @@ROWCOUNT = 0 BREAK;
END
