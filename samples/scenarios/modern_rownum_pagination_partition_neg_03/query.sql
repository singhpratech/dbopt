-- PARTITION BY numbers rows per group (per-event ordinal); a range filter on
-- it is grouping logic, not pagination.
SELECT d.event_date, d.qn, d.dn
FROM
(
    SELECT dp.event_date,
           qn = ROW_NUMBER() OVER (PARTITION BY dp.event_date ORDER BY dp.event_date) - 1,
           dn = ROW_NUMBER() OVER (PARTITION BY dp.event_date, dp.id ORDER BY dp.event_date)
    FROM #deadlock_process AS dp
) AS d
WHERE d.dn > 1 AND d.dn <= 5;
