SELECT @Id = ID FROM @tmpDatabases WHERE Selected = 1
UPDATE tmpDatabases
SET tmpDatabases.StartPosition = @StartPosition, tmpDatabases.Selected = 1
FROM @tmpDatabases AS tmpDatabases
INNER JOIN dbo.Databases AS d ON d.ID = tmpDatabases.ID
SELECT TOP 1 @CurrentDBID = ID, @CurrentDatabaseName = DatabaseName FROM @tmpDatabases
