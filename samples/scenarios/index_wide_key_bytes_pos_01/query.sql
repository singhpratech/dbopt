-- Three key columns, but NVARCHAR(100) + UNIQUEIDENTIFIER + BIGINT is ~224
-- bytes per key — every nonclustered index inherits all of it.
CREATE TABLE dbo.TenantEvents (
    TenantCode nvarchar(100)    NOT NULL,
    EventGuid  uniqueidentifier NOT NULL,
    Sequence   bigint           NOT NULL,
    Payload    nvarchar(2000)   NULL,
    CONSTRAINT PK_TenantEvents PRIMARY KEY CLUSTERED (TenantCode, EventGuid, Sequence)
);
