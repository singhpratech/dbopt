-- `GO` is a batch separator. The CREATE INDEX after it is DDL, so `ON dbo.V (`
-- introduces a target table, not a scalar UDF call.
SELECT 1;
GO
CREATE UNIQUE CLUSTERED INDEX IX_v ON dbo.SomeView (ProductID);
