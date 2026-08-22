-- The column is wrapped in a function, but it is still the column being
-- searched with a leading wildcard.
SELECT CustomerId FROM dbo.Customers WHERE LTRIM(RTRIM(Email)) LIKE N'%@example.com';
