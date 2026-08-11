-- A comment between EXISTS's paren and its SELECT does not change that the
-- column list is never evaluated.
SELECT o.OrderId
FROM dbo.Orders o
WHERE EXISTS ( -- only orders that have lines
    SELECT * FROM dbo.OrderLines l WHERE l.OrderId = o.OrderId
);
