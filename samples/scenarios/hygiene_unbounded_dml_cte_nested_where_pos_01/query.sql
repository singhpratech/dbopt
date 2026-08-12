-- The WHERE bounds a nested scalar subquery, not the CTE itself.
WITH q AS (
  SELECT f, (SELECT MAX(x) FROM dbo.U u WHERE u.id = t.id) AS m
  FROM dbo.T t
)
UPDATE q SET f = 1;
