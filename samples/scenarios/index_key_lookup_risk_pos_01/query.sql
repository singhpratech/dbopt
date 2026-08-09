-- A seek exists on Status but carries no INCLUDE, so the four other projected
-- columns each cost a key lookup.
CREATE NONCLUSTERED INDEX IX_Orders_Status ON dbo.Orders ([Status]);

SELECT o.Id, o.Total, o.CustId, o.CreatedAt, o.Region
FROM dbo.Orders o
WHERE o.Status = 'OPEN';
