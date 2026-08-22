-- ORDER BY after a GROUP BY sorts the aggregate output; no index on the base
-- table returns grouped rows pre-ordered, so the sort-avoiding index is moot.
SELECT  s.Region, s.Channel, SUM(s.Amount) AS Total
FROM    dbo.Sales AS s
WHERE   s.TenantId = 7
GROUP BY s.Region, s.Channel
ORDER BY s.Region, s.Channel;
