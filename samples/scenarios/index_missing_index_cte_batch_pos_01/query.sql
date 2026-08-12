-- A CTE does not survive GO. `Recent` in the second batch is a real table and
-- must still get index advice. The CTE body carries no WHERE clause, so the
-- only statement that can satisfy must_fire is the batch-2 one — otherwise the
-- guard passes with the bug present.
WITH Recent AS (SELECT Id FROM dbo.Events)
SELECT Id FROM Recent;
GO
SELECT OrderId, Total FROM dbo.Recent WHERE CustomerId = 42;
GO
