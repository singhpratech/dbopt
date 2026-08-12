-- The later PK is explicitly NONCLUSTERED, so the table really is a heap.
CREATE TABLE dbo.Staging (Id int NOT NULL);
GO
ALTER TABLE dbo.Staging ADD CONSTRAINT PK_S PRIMARY KEY NONCLUSTERED (Id);
GO
