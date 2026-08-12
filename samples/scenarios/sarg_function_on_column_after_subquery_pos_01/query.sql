-- A nested SELECT inside the predicate must not close the region either.
SELECT Id FROM dbo.T
WHERE id IN (SELECT id FROM dbo.U) AND UPPER(Name) = 'X';
