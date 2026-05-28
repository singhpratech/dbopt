SELECT *
FROM dbo.Customers c
WHERE dbo.fnFullName(c.FirstName, c.LastName) = 'Alice Smith';