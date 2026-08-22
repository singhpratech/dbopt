-- TOP 100 PERCENT bounds nothing: the whole rowset is still sorted, so
-- the unbounded-sort warning must still fire.
SELECT TOP 100 PERCENT o.OrderId, o.CustomerId, o.TotalCents
FROM   dbo.Orders AS o
ORDER BY o.OrderDate;
