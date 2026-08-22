-- NULL-guard idioms: each OR makes a filter optional, it is not a value list
-- that a UNION could split.
SELECT t.DatabaseName
FROM dbo.Databases AS t
JOIN dbo.SelectedDatabases AS s
  ON t.DatabaseName LIKE s.DatabaseName
 AND (t.DatabaseType = s.DatabaseType OR s.DatabaseType IS NULL)
 AND (t.AvailabilityGroup = s.AvailabilityGroup OR s.AvailabilityGroup IS NULL)
WHERE (t.IncludedColumns IS NULL OR CHARINDEX(t.IncludedColumns, s.IncludedColumns) > 0)
  AND ((@SqlHandle IS NOT NULL AND t.SqlHandle = @SqlHandle)
    OR (@SqlHandle IS NULL AND t.SqlHandle IS NULL));
