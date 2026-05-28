-- Version-silence: a recursive numbers CTE (UNION ALL + self-reference) is the
-- shape modern.generate_series_replaces_numbers_cte fires on (GENERATE_SERIES is
-- 2022+). Target is 2017, below the 2022 gate, so the rule must stay silent.
WITH Numbers AS
(
    SELECT 1 AS n
    UNION ALL
    SELECT n + 1 FROM Numbers WHERE n < 10000
)
SELECT n FROM Numbers OPTION (MAXRECURSION 0);
