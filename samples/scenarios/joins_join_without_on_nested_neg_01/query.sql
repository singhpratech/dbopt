-- Nested-join syntax: the inner join's ON comes first, then the outer's.
SELECT OBJECT_NAME(newtbl.object_id) AS TableName
FROM sys.objects AS constraints
JOIN sys.extended_properties AS p
JOIN sys.objects AS newtbl
  ON newtbl.object_id = p.major_id
 AND p.minor_id = 0
  ON OBJECT_NAME(constraints.parent_object_id) = CAST(p.value AS nvarchar(4000))
 AND constraints.schema_id = newtbl.schema_id;
