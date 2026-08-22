-- ROW_NUMBER() as a ranking / sort key and as a keep-one-per-group dedup:
-- neither slices a page, so no OFFSET/FETCH rewrite applies.
INSERT INTO #findings (sort_order, finding)
SELECT sort_order = ROW_NUMBER() OVER (ORDER BY COUNT_BIG(d.id) DESC),
       d.finding
FROM #deadlocks AS d
GROUP BY d.finding;

WITH c AS (
    SELECT *, rn = ROW_NUMBER() OVER (PARTITION BY owner_id ORDER BY event_date)
    FROM #owners
)
DELETE c
WHERE c.rn > 1;
