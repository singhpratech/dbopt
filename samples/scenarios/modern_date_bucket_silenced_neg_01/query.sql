-- Version-silence: the DATEADD(... DATEDIFF(...) / N ...) floor-bucketing idiom
-- is what modern.date_bucket_replaces_floor_datediff fires on (DATE_BUCKET is
-- 2022+). Target is 2017, below the 2022 gate, so the rule must stay silent.
SELECT  DATEADD(MINUTE, (DATEDIFF(MINUTE, 0, e.EventTime) / 5) * 5, 0) AS Bucket,
        COUNT(*) AS Hits
FROM    dbo.Events AS e
GROUP BY DATEADD(MINUTE, (DATEDIFF(MINUTE, 0, e.EventTime) / 5) * 5, 0);
