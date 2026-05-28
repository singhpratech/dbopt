-- GUID clustered key with NEWID() default. Every insert lands at a random
-- B-tree leaf, fragmenting the table, blowing up page splits, and bloating
-- every nonclustered index (which carries the clustering key). Use NEWSEQUENTIALID
-- or, better, a bigint identity / sequence.
CREATE TABLE dbo.Audit (
    Id          uniqueidentifier NOT NULL
                CONSTRAINT PK_Audit PRIMARY KEY CLUSTERED
                DEFAULT NEWID(),
    EventType   varchar(64)      NOT NULL,
    Payload     nvarchar(max)    NULL,
    CreatedAt   datetime2(3)     NOT NULL CONSTRAINT DF_Audit_CreatedAt DEFAULT SYSUTCDATETIME()
);
