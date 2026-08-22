-- Arithmetic on the column side of a JOIN ON and inside an IF EXISTS subquery
-- are real predicates and must still be reported after the scoping change.
SELECT o.OrderId
FROM dbo.Orders AS o
JOIN dbo.Lines AS l ON l.Qty * 2 > o.Threshold
WHERE o.Total - 10 >= @min;
IF EXISTS (SELECT 1 FROM dbo.Orders WHERE Qty + 1 = 5)
    PRINT 'found';
