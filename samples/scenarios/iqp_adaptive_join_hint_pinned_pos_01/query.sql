-- Hand-pinned LOOP join. Once upon a time someone fixed a regression by
-- forcing nested loops; the workload has since drifted and an adaptive
-- join (IQP, 2017+) or hash join would be much better. Pinning the join
-- type also disables batch-mode adaptive joins on 2019+.
SELECT  o.OrderId,
        o.TotalCents,
        c.CustomerId,
        c.Email
FROM    dbo.Orders    AS o
INNER LOOP JOIN dbo.Customers AS c
    ON  c.CustomerId = o.CustomerId
WHERE   o.OrderDate >= DATEADD(DAY, -30, GETDATE());
