-- Recursive CTEs that walk a blocking chain and split a CSV string: both are
-- UNION ALL self-references, neither is a numbers table GENERATE_SERIES can replace.
WITH blockers AS (
    SELECT s.session_id, s.blocking_session_id, level = 1
    FROM #sessions AS s
    WHERE s.blocking_session_id = 0
    UNION ALL
    SELECT s.session_id, s.blocking_session_id, b.level + 1
    FROM blockers AS b
    JOIN #sessions AS s ON s.blocking_session_id = b.session_id
)
SELECT * FROM blockers;

WITH parts AS (
    SELECT CAST(1 AS bigint) AS f, CHARINDEX(',', @list) AS t
    UNION ALL
    SELECT t + 1, CHARINDEX(',', @list, t + 1)
    FROM parts
    WHERE CHARINDEX(',', @list, t + 1) > 0
)
SELECT SUBSTRING(@list, f, t - f) FROM parts;
