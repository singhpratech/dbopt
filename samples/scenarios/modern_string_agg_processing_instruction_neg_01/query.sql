-- FOR XML PATH('') used to build real XML: a processing-instruction column for
-- click-to-view text, and nested element/attribute paths. STRING_AGG cannot
-- produce either.
SELECT [processing-instruction(query)] = SUBSTRING(st.text, 1, 4000)
FROM sys.dm_exec_sql_text(@handle) AS st
FOR XML PATH(''), TYPE;

SELECT
    l1.database_name AS [Database/@name],
    (
        SELECT l2.request_mode AS [Lock/@request_mode],
               COUNT(*) AS [Lock/@request_count]
        FROM #locks AS l2
        WHERE l1.session_id = l2.session_id
        GROUP BY l2.request_mode
        FOR XML PATH(''), TYPE
    ) AS [Database/Locks]
FROM #locks AS l1
GROUP BY l1.database_name, l1.session_id
FOR XML PATH('Database'), TYPE;
