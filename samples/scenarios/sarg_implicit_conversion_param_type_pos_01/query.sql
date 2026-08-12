-- The classic silently-lost seek: an ANSI column filtered by a Unicode
-- parameter. nvarchar outranks varchar, so the COLUMN is converted on every
-- row and the index on Email cannot be seeked.
CREATE TABLE dbo.Customers (CustomerId int NOT NULL, Email varchar(200) NOT NULL);
GO
CREATE PROCEDURE dbo.FindCustomer @Email nvarchar(200)
AS
SET NOCOUNT ON;
SELECT CustomerId FROM dbo.Customers WHERE Email = @Email;
