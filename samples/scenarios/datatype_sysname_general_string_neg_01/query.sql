-- sysname used for what it is: an identifier column mirroring a catalog name.
CREATE TABLE dbo.TrackedTables (TrackedId int NOT NULL, table_name sysname NOT NULL);
