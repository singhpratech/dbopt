-- Every branch reads the same temp table and projects the same single column,
-- differing only in ORDER BY: the top-N sets are guaranteed to overlap and the
-- dedupe is the point. UNION ALL would return duplicate hashes.
SELECT qs.query_hash
FROM
(
    SELECT TOP (@top) qs.query_hash
    FROM #hi_query_stats AS qs
    ORDER BY qs.total_cpu_ms DESC

    UNION

    SELECT TOP (@top) qs.query_hash
    FROM #hi_query_stats AS qs
    ORDER BY qs.total_duration_ms DESC

    UNION

    SELECT TOP (@top) qs.query_hash
    FROM #hi_query_stats AS qs
    WHERE qs.total_tempdb_mb > 0
    ORDER BY qs.total_tempdb_mb DESC
) AS x;
