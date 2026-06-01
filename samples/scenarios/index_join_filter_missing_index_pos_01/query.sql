-- Clean two-table INNER equijoin with a sargable equality filter on the joined
-- (probed) table. The engine seeks into Orders once per matching Customer; a
-- covering index on (the equality-filter column, then the join key) with the
-- projected columns INCLUDEd turns that probe into a seek.
SELECT  c.Name,
        o.OrderId,
        o.TotalCents
FROM    dbo.Customers AS c
INNER JOIN dbo.Orders AS o ON o.CustomerId = c.CustomerId
WHERE   o.Status = 'OPEN';
