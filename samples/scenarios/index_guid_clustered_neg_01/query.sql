-- False-positive guard: a clustered primary key on a sequential BIGINT IDENTITY
-- surrogate -- not a uniqueidentifier. Inserts append at the right edge with no
-- page-split risk, so index.guid_clustered_key must stay silent. (The unique
-- GUID lives in a nonclustered unique constraint, which is the recommended
-- pattern.)
CREATE TABLE dbo.Audit (
    Id          bigint           IDENTITY(1,1) NOT NULL
                CONSTRAINT PK_Audit PRIMARY KEY CLUSTERED,
    ExternalRef uniqueidentifier NOT NULL
                CONSTRAINT UQ_Audit_ExternalRef UNIQUE,
    EventType   varchar(64)      NOT NULL,
    CreatedAt   datetime2(3)     NOT NULL
);
