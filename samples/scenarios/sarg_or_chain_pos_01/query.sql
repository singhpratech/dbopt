SELECT OrderId
FROM dbo.Orders
WHERE Status = 1
   OR Status = 2
   OR Status = 3
   OR Status = 4
   OR Status = 5;