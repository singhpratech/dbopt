-- The aggregation reads a CTE over shredded plan XML; there is no base table
-- to put a columnstore on.
WITH IndexOps AS (
    SELECT  q.QueryHash,
            q.n.value('@Index', 'nvarchar(128)') AS IndexName
    FROM    #plans AS q
    CROSS APPLY q.plan_xml.nodes('//IndexScan') AS q(n)
)
SELECT  ios.QueryHash,
        COUNT(*)          AS index_ops,
        SUM(CASE WHEN ios.IndexName IS NULL THEN 1 ELSE 0 END) AS unnamed
FROM    IndexOps AS ios
GROUP BY ios.QueryHash;
