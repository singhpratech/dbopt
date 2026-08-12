-- The reverse direction is harmless and must stay silent: a Unicode column
-- against an ANSI parameter converts the PARAMETER once, and the seek survives.
CREATE TABLE dbo.Customers (CustomerId int NOT NULL, Email nvarchar(200) NOT NULL);
GO
CREATE PROCEDURE dbo.FindCustomer @Email varchar(200)
AS
SET NOCOUNT ON;
SELECT CustomerId FROM dbo.Customers WHERE Email = @Email;
