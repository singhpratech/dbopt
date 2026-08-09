-- The batch declares a covering index keyed on the filter column. The rule
-- claims "no matching index is declared in this batch", so it must stay silent
-- here or the message is a lie.
CREATE NONCLUSTERED INDEX IX_Orders_Status
    ON dbo.Orders ([Status])
    INCLUDE ([Id], [Total]);

SELECT o.Id, o.Total
FROM dbo.Orders o
WHERE o.Status = 'OPEN';
