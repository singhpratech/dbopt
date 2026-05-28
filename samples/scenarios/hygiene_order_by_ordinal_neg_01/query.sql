-- ORDER BY uses explicit column names, not positions.
SELECT FirstName, LastName FROM dbo.Customers ORDER BY LastName, FirstName;
