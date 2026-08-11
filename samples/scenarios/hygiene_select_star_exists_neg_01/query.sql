-- SELECT * inside EXISTS is the documented idiomatic form: the column list is
-- never evaluated, so there is no read amplification to warn about.
SELECT c.Id
FROM dbo.Customers AS c
WHERE EXISTS (SELECT * FROM dbo.Orders AS o WHERE o.CustomerId = c.Id);
