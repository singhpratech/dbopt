CREATE TABLE dbo.Document (
    DocumentNode hierarchyid NOT NULL,
    [DocumentLevel] AS DocumentNode.GetLevel(),
    Title nvarchar(50) NOT NULL
);
