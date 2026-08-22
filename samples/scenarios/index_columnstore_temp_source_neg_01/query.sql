-- SUM ... GROUP BY over a #temp table a diagnostic proc filled moments ago.
-- Session-scoped work tables are not columnstore candidates.
SELECT  wa.wait_type,
        SUM(wa.wait_time_ms) AS total_wait_ms,
        COUNT(*)             AS samples
FROM    #waits_agg AS wa
GROUP BY wa.wait_type;
