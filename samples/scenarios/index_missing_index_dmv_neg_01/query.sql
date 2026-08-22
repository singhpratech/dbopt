-- msdb system tables and INFORMATION_SCHEMA views are not yours to index;
-- `CREATE INDEX ... ON msdb.dbo.sysjobhistory` is DDL nobody should run.
SELECT TOP 1 h.run_status, h.run_date, h.message
FROM   msdb.dbo.sysjobhistory AS h
WHERE  h.job_id = @job_id
  AND  h.step_id = 0;

IF EXISTS (SELECT c.COLUMN_NAME
           FROM   msdb.INFORMATION_SCHEMA.COLUMNS AS c
           WHERE  c.TABLE_NAME = 'backupset' AND c.COLUMN_NAME = 'encryptor_thumbprint')
    PRINT 'encryption columns present';

SELECT l.name, l.sysadmin
FROM   master.dbo.syslogins AS l
WHERE  l.name = @login AND l.denylogin = 0;
