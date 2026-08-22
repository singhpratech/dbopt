-- IDENTITY and PRIMARY KEY columns are NOT NULL by definition; their
-- nullability never falls back to ANSI_NULL_DFLT_ON.
CREATE TABLE dbo.JobLog (
    job_id    smallint IDENTITY(1,1) PRIMARY KEY CLUSTERED,
    plan_id   bigint   NOT NULL,
    message   nvarchar(4000) NULL
);
CREATE TABLE dbo.BlitzResults (
    ID      INT IDENTITY(1,1),
    Finding NVARCHAR(200) NOT NULL
);
