-- TOP 1 ... ORDER BY is a trivial top-N; the engine keeps one row, not a sort
-- buffer, and this is the idiomatic "latest backup" lookup on msdb.
SELECT TOP 1 b.backup_start_date
FROM   msdb.dbo.backupset AS b
WHERE  b.database_name = @db AND b.type = 'D'
ORDER BY b.backup_start_date DESC;

SELECT TOP (1) o.OrderId
FROM   dbo.Orders AS o
WHERE  o.CustomerId = 42
ORDER BY o.PlacedAt DESC;
