-- Aliases declared inside the statement resolve to their catalog source even
-- when the same alias letter names a user table elsewhere in the file.
SELECT c.CustomerId FROM dbo.Customers AS c WHERE c.Region = 'EU';

SELECT o.object_id
FROM sys.objects AS o
JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE o.name = N'DeadLockTbl' AND s.name = N'dbo' AND o.type_desc = N'SYNONYM';

IF EXISTS (SELECT 1 FROM sys.configurations AS c WHERE c.name = N'blocked process threshold (s)')
    PRINT 'set';

SELECT j.job_id FROM msdb.dbo.sysjobs AS j WHERE j.name = N'nightly';

SELECT fmp.permission_name
FROM fn_my_permissions(N'msdb.dbo.sysjobsteps', N'OBJECT') AS fmp
WHERE fmp.permission_name = N'SELECT';
