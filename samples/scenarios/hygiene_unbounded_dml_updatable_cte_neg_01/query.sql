-- The updatable-CTE idiom: the CTE body carries the predicate, so the UPDATE is
-- bounded by it.
WITH tmpDatabases AS (
  SELECT DatabaseName, [Order], ROW_NUMBER() OVER (ORDER BY DatabaseName ASC) AS RowNumber
  FROM @tmpDatabases
  WHERE Selected = 1
)
UPDATE tmpDatabases SET [Order] = RowNumber;
