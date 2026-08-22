-- The lead key `database_id` is an INT (it mirrors sys.databases.database_id),
-- not a GUID: an `_id` suffix says nothing about randomness of inserts.
CREATE TABLE dbo.IndexCleanupResults (
    database_id   int          NOT NULL,
    database_name sysname      NOT NULL,
    object_id     int          NOT NULL,
    index_name    sysname      NULL
);
CREATE CLUSTERED INDEX c ON dbo.IndexCleanupResults (database_id, object_id);
