-- Every one of these was introduced after SQL Server 2014, so on a 2014
-- target the batch does not compile. compat.unsupported_on_target must fire.
SELECT STRING_AGG(p.Name, ',') FROM dbo.Products AS p;
SELECT GREATEST(1, 2, 3), LEAST(4, 5), DATETRUNC(month, SYSDATETIME()), JSON_OBJECT('a': 1);
SELECT TRIM(c.Name), CONCAT_WS('-', c.FirstName, c.LastName) FROM dbo.Customers AS c;
SELECT value FROM STRING_SPLIT('a,b,c', ',');
SELECT j.[key] FROM OPENJSON('{"a":1}') AS j;
SELECT APPROX_COUNT_DISTINCT(o.CustomerId) FROM dbo.Orders AS o;
DROP TABLE IF EXISTS dbo.Staging;
