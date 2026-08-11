-- A CTE does not survive GO. `Recent` in the second batch is a real table and
-- must still get index advice.
WITH Recent AS (SELECT Id FROM dbo.Events WHERE Id > 0)
SELECT Id FROM Recent;
GO
SELECT OrderId, Total FROM dbo.Recent WHERE CustomerId = 42;
GO
