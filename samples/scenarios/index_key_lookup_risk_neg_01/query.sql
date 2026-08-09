-- The declared index already covers every projected column, so there is no
-- lookup to warn about and nothing to recommend.
CREATE NONCLUSTERED INDEX IX_Orders_Status
    ON dbo.Orders ([Status])
    INCLUDE ([Id], [Total], [CustId], [CreatedAt], [Region]);

SELECT o.Id, o.Total, o.CustId, o.CreatedAt, o.Region
FROM dbo.Orders o
WHERE o.Status = 'OPEN';
