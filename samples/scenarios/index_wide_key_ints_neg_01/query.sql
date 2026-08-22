-- Four INT key columns are 16 bytes: that is a narrow key however many
-- columns it has. Width is measured in bytes, not column count.
CREATE TABLE dbo.ComputedColumnsAnalysis (
    database_id int NOT NULL,
    schema_id   int NOT NULL,
    object_id   int NOT NULL,
    column_id   int NOT NULL,
    definition  nvarchar(4000) NULL,
    PRIMARY KEY CLUSTERED (database_id, schema_id, object_id, column_id)
);
