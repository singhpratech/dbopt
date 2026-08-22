-- String building in a select-list CASE, a SET and a PRINT never touches an
-- index. Only a concatenation on the column side of a search condition does.
SELECT CASE WHEN s.is_disabled = 1
            THEN N'--ALTER TABLE ' + QUOTENAME(s.schema_name) + N'.' + QUOTENAME(s.table_name)
               + N' DROP CONSTRAINT ' + QUOTENAME(s.index_name) + N';'
            ELSE N'' END AS drop_script,
       'Corruption check of ' + DB_NAME(db.database_id) + ' database (' + CAST(db.pct AS varchar(10)) + '%)' AS Details
FROM #stats AS s
JOIN #dbs AS db ON db.database_id = s.database_id
WHERE s.index_id > 0;
SET @Details = CASE WHEN trace_flags_session IS NOT NULL THEN ', Session Level Trace Flag(s) Enabled: ' + trace_flags_session ELSE '' END;
PRINT '**** ' + CHAR(10) + '**** $(DatabaseName) Database does not exist: ' + @DatabaseName;
