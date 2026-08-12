-- Both sides declared, same family — nothing converts.
CREATE TABLE dbo.Customers (CustomerId int NOT NULL, Email varchar(200) NOT NULL);
GO
CREATE PROCEDURE dbo.FindCustomer @Email varchar(200)
AS
SET NOCOUNT ON;
SELECT CustomerId FROM dbo.Customers WHERE Email = @Email;
