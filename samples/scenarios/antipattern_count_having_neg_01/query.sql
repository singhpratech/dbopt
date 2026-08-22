-- HAVING COUNT(*) > 0 after GROUP BY is a per-group predicate, not an
-- existence test; and a CASE that returns the count it tests must compute it anyway.
INSERT INTO #results (database_name, n)
SELECT d.database_name, COUNT(*)
FROM #sessions AS d
WHERE d.application_name NOT LIKE '%Monitor%'
GROUP BY d.database_name
HAVING COUNT(*) > 0;

SELECT CASE WHEN (SELECT COUNT(*) FROM @Directories WHERE Mirror = 0) > 0
            THEN (SELECT COUNT(*) FROM @Directories WHERE Mirror = 0)
            ELSE (SELECT COUNT(*) FROM @URLs WHERE Mirror = 0) END AS NumberOfDirectories;
