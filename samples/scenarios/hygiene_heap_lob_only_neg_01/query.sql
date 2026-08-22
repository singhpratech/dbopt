-- Every column is a LOB / MAX type: none can be a clustered key, so "add a
-- clustered index" is advice nobody can take.
CREATE TABLE dbo.RawPayloads (
    payload varbinary(max) NOT NULL,
    body    nvarchar(max)  NULL
);
