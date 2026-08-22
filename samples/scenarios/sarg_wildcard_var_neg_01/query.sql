-- Every LIKE here tests a local VARIABLE (directly, or an expression built
-- from one) in an IF or a select-list CASE. No column, no index.
IF @CurrentDatabaseName LIKE '%"%'
    SET @CurrentDatabaseName = REPLACE(@CurrentDatabaseName, '"', '');
IF REPLACE(REPLACE(@AvailabilityGroupDirectoryStructure, '{ClusterName}', ''), '{AvailabilityGroupName}', '') LIKE '%{%'
    RAISERROR('Unknown token', 16, 1);
SET @sql = @sql + CASE WHEN @output_column_list LIKE '%|[tempdb_allocations_delta|]%' ESCAPE '|' THEN N', tempdb_allocations_delta' ELSE N'' END;
