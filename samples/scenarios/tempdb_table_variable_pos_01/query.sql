-- Table variable used as a working set for a downstream join.
-- Pre-2019 the optimizer estimates @t as a single row, which leads to
-- nested-loops plans against multi-million-row Orders. Even with deferred
-- compilation (2019+) the lack of column statistics still hurts.
DECLARE @t TABLE (
    id int NOT NULL PRIMARY KEY
);

INSERT INTO @t (id)
SELECT  o.OrderId
FROM    dbo.Orders AS o
WHERE   o.OrderDate >= DATEADD(DAY, -1, GETDATE());

SELECT  o.OrderId,
        o.CustomerId,
        o.TotalCents
FROM    dbo.Orders AS o
JOIN    @t          AS t ON t.id = o.OrderId;
