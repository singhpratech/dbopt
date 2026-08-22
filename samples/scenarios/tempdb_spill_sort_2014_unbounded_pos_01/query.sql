-- Whole-table sort with no TOP / OFFSET / equality filter / GROUP BY on a
-- 2014 target (no Memory Grant Feedback): the genuine warning case.
SELECT o.OrderId, o.CustomerId, o.OrderDate
FROM   dbo.Orders AS o
WHERE  o.OrderDate >= '2014-01-01'
ORDER BY o.TotalCents DESC;
