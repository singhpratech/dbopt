-- Batched purge that relies on SET ROWCOUNT to limit the DELETE. SET ROWCOUNT
-- no longer affects DML on supported versions and will be removed entirely; the
-- DELETE here actually rewrites every matching row.
SET ROWCOUNT 5000;

DELETE FROM dbo.AuditLog
WHERE CreatedAt < DATEADD(DAY, -90, SYSUTCDATETIME());

SET ROWCOUNT 0;
