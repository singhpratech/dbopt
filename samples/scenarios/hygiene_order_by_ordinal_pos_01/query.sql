-- ORDER BY column ordinals break silently when the SELECT list is edited.
SELECT FirstName, LastName FROM dbo.Customers ORDER BY 1, 2;
