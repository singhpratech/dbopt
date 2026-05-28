-- Unbounded ORDER BY on a wide rowset. With no TOP/OFFSET and no supporting
-- index on OrderDate, the optimizer has to materialize the entire result set
-- into a Sort operator, which often spills to tempdb on production volumes.
SELECT  o.OrderId,
        o.CustomerId,
        o.TotalCents,
        o.OrderDate,
        o.Status
FROM    dbo.Orders AS o
ORDER BY o.OrderDate;
