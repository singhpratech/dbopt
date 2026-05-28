-- Wide composite clustered primary key. Every nonclustered index on this table
-- silently appends all four clustering columns, inflating each index's leaf and
-- non-leaf pages. A narrow surrogate IDENTITY clustered key avoids the bloat.
CREATE TABLE dbo.LineItems (
    TenantId   int           NOT NULL,
    OrderId    int           NOT NULL,
    LineNo     int           NOT NULL,
    SkuCode    varchar(40)   NOT NULL,
    Quantity   int           NOT NULL,
    CONSTRAINT PK_LineItems PRIMARY KEY CLUSTERED (TenantId, OrderId, LineNo, SkuCode)
);
