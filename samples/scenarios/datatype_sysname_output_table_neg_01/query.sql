-- Every column holds an object identifier later fed to QUOTENAME/OBJECT_ID.
CREATE TABLE #targets (
    output_database sysname NOT NULL,
    output_schema sysname NOT NULL,
    output_table sysname NOT NULL,
    temp_table sysname NOT NULL,
    built_on sysname NULL,
    view_id sysname NULL
);
