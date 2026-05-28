-- Hot recent-window query against an ascending OrderDate index.
-- Statistics histograms on monotonically increasing keys typically lag
-- behind the actual maximum, causing the optimizer to underestimate
-- cardinality for the trailing range. Future rule will flag this.
SELECT  o.OrderId,
        o.CustomerId,
        o.TotalCents
FROM    dbo.Orders AS o
WHERE   o.OrderDate >= DATEADD(DAY, -7, GETDATE())
ORDER BY o.OrderDate DESC;
