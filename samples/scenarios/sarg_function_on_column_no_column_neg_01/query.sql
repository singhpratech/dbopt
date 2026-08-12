-- Neither predicate wraps a column: a datepart keyword and a CAST target type
-- are not columns, and there is no index to lose.
SELECT 1 WHERE DATEDIFF(MM, @VersionDate, GETDATE()) > 6;
SELECT 1 WHERE CAST(SERVERPROPERTY('edition') AS VARCHAR(100)) LIKE '%Developer%';
