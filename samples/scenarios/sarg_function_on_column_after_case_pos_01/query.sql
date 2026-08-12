-- A CASE ... END inside the predicate must not close the predicate region: the
-- non-SARGable UPPER() after it is real.
SELECT Id FROM dbo.T
WHERE CASE WHEN a = 1 THEN 1 ELSE 0 END = 1 AND UPPER(col) = 'X';
