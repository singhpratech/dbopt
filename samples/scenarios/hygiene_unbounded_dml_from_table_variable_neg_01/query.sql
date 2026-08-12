-- The target is an alias bound by the FROM clause to a table variable.
UPDATE tmpDatabases
SET AvailabilityGroup = 1
FROM @tmpDatabases tmpDatabases;
