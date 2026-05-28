-- Classic "numbers table on demand" via recursive CTE. Works, but:
--   * recursive CTEs run as one-row-at-a-time nested loops
--   * MAXRECURSION 0 disables the safety limit
-- SQL Server 2022 ships GENERATE_SERIES, which is a native set-returning
-- function with proper cardinality estimates.
WITH Numbers AS (
    SELECT 1 AS n
    UNION ALL
    SELECT n + 1
    FROM   Numbers
    WHERE  n < 1000
)
SELECT n
FROM   Numbers
OPTION (MAXRECURSION 0);
