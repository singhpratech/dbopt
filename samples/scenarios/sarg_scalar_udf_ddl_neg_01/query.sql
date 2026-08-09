-- `CREATE INDEX ... ON dbo.Orders ([Status])` puts `dbo.Orders (` after ON,
-- which has the same token shape as a scalar UDF call. DDL must not be read as
-- a predicate.
CREATE NONCLUSTERED INDEX IX_Orders_Status
    ON dbo.Orders ([Status])
    INCLUDE ([Id]);
