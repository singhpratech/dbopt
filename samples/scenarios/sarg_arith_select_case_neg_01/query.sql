-- `x.page_no % 8088 = 0` classifies pages inside a CASE in the SELECT list
-- (and in an UPDATE SET list): a computed value, not a search predicate.
SELECT x.page_no,
       CASE WHEN x.page_no = 1 OR x.page_no % 8088 = 0 THEN 'PFS'
            WHEN x.page_no = 2 OR x.page_no % 511232 = 0 THEN 'GAM'
            ELSE '*' END AS page_type
FROM #pages AS x;
UPDATE c
SET is_bad_estimate = CASE WHEN c.estimated_rows * 1000 < c.returned_rows THEN 1 ELSE 0 END
FROM #cache AS c
WHERE c.query_hash IS NOT NULL;
