-- Anchored prefix LIKE — perfectly seekable.
SELECT CustomerId, Email
FROM dbo.Customers
WHERE Email LIKE 'alice%';
