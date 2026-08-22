-- The column name gives no hint, but the declared type is UNIQUEIDENTIFIER
-- with a NEWID() default: random inserts with no FILLFACTOR slack.
CREATE TABLE dbo.Sessions (
    SessionKey uniqueidentifier NOT NULL DEFAULT NEWID(),
    UserId     int              NOT NULL,
    StartedAt  datetime2(3)     NOT NULL
);
CREATE CLUSTERED INDEX CIX_Sessions ON dbo.Sessions (SessionKey);
