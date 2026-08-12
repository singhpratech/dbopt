-- DDL outside any explicit transaction holds no schema lock across DML.
CREATE TABLE dbo.StagingRows (Id int NOT NULL, Payload nvarchar(400) NOT NULL);
GO
BEGIN TRANSACTION;
INSERT INTO dbo.StagingRows (Id, Payload) VALUES (1, N'x');
COMMIT TRANSACTION;
