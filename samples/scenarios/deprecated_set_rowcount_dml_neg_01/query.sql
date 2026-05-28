-- False-positive guard: the modern batched-purge pattern uses TOP (n) on the
-- DELETE itself rather than the deprecated SET ROWCOUNT. There is no SET ROWCOUNT
-- anywhere, so deprecated.set_rowcount_dml must stay silent.
DELETE TOP (5000) FROM dbo.AuditLog
WHERE CreatedAt < DATEADD(DAY, -90, SYSUTCDATETIME());
