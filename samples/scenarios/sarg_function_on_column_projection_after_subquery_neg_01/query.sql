-- A WHERE inside a subquery ends when that subquery's parens close. Latching
-- the predicate region open made every later projection look like a predicate.
SELECT CASE WHEN id IN (SELECT id FROM dbo.Archive WHERE flag = 1) THEN 1
            WHEN UPPER(status) = 'OPEN' THEN 2 END AS Bucket
FROM dbo.T;
