-- A point-filtered ordered result on a 2014 target. The rule still notes the
-- unbounded ORDER BY, but as info: the equality predicate bounds the rowset,
-- so the old "warning on every ORDER BY for 2014/2016" escalation must not apply.
SELECT o.OrderId, o.OrderDate, o.TotalCents
FROM   dbo.Orders AS o
WHERE  o.CustomerId = 5
ORDER BY o.OrderDate DESC;
