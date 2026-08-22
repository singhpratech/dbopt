-- Comment / string / user-object immunity on a 2014 target: the names appear
-- only where they are not built-in calls, so the batch compiles fine on 2014.
-- GREATEST(1,2) and DATETRUNC(month, x) are mentioned here, in a comment.
/* STRING_AGG(x, ',') and DROP TABLE IF EXISTS dbo.t live in this block comment. */
SELECT c.CustomerId,
       'TRIM(name)' AS Literal,
       dbo.TRIM(c.Name) AS UserFn,
       c.[GREATEST] AS OddColumn
FROM   dbo.Customers AS c
WHERE  c.Note = 'JSON_OBJECT(''a'':1)';
