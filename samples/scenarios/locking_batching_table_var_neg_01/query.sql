-- UPDATE through an alias whose source is a table variable. Table variables
-- are session-private: lock escalation on one blocks nobody, so batching
-- advice cannot be acted on.
UPDATE tmpDatabases
SET    tmpDatabases.Selected = 1
FROM   @tmpDatabases AS tmpDatabases
INNER JOIN @SelectedDatabases AS SelectedDatabases
    ON tmpDatabases.DatabaseName LIKE REPLACE(SelectedDatabases.DatabaseName, '_', '[_]')
WHERE  SelectedDatabases.Selected = 1
  AND  tmpDatabases.DatabaseSize >= @MinSize;
