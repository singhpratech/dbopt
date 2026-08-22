-- Two shapes that are not a substring search on an indexed column: a COLUMN
-- as the needle (no LIKE-literal rewrite exists) and a VARIABLE as the
-- haystack (no column, no index — a string-split CTE).
SELECT ia1.index_name
FROM #index_analysis AS ia1
JOIN #index_analysis AS ia2 ON ia2.table_id = ia1.table_id
WHERE CHARINDEX(ia1.included_columns, ia2.included_columns) > 0;
WITH Split AS (
    SELECT CHARINDEX(',', @FilterPlansByDatabase) AS t
    UNION ALL
    SELECT CHARINDEX(',', @FilterPlansByDatabase, t + 1)
    FROM Split
    WHERE CHARINDEX(',', @FilterPlansByDatabase, t + 1) > 0
)
SELECT t FROM Split;
