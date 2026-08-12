-- An explicit, bounded MAXDOP.
EXEC sp_configure 'max degree of parallelism', 8;
RECONFIGURE;
