-- An `=` in a SET clause is an assignment, not a comparison: no index is
-- consulted and no per-row column conversion happens.
CREATE TABLE dbo.Users (UserId int NOT NULL, Email varchar(200) NOT NULL);
GO
CREATE PROCEDURE dbo.SetEmail @Email nvarchar(200)
AS
SET NOCOUNT ON;
UPDATE dbo.Users SET Email = @Email WHERE UserId = 1;
