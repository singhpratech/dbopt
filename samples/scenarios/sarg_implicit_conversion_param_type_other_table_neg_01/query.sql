-- Email is declared on dbo.Archive. The query filters a different table, so we
-- know nothing about the type of the column it actually touches.
CREATE TABLE dbo.Archive (Email varchar(200) NOT NULL);
GO
CREATE PROCEDURE dbo.FindLive @Email nvarchar(200)
AS
SET NOCOUNT ON;
SELECT LiveId FROM dbo.LiveUsers AS lu WHERE lu.Email = @Email;
