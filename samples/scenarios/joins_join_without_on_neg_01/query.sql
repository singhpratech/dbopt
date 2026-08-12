-- CROSS JOIN is the explicit, intentional form and needs no ON.
SELECT d.DayDate, s.StoreId
FROM dbo.Calendar AS d
CROSS JOIN dbo.Stores AS s;
