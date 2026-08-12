-- Same shape, but the CTE body has no predicate: this really does rewrite every
-- row of the base table, and must still be reported.
WITH q AS (SELECT Flag FROM dbo.RealTable)
UPDATE q SET Flag = 1;
