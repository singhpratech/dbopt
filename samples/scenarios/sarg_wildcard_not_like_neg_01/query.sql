-- A negated LIKE can never seek, whatever the wildcard position: it is a
-- residual filter by nature, so "leading wildcard lost the seek" is wrong.
SELECT d.object_id
FROM dbo.DbccEvents AS d
WHERE d.is_ms_shipped = 0
  AND d.dbcc_event_full_upper NOT LIKE '%DBCC%CHECKIDENT%'
  AND d.dbcc_event_full_upper NOT LIKE '%DBCC%CHECKTABLE%'
  AND d.DatabaseName NOT LIKE '%[%]%';
