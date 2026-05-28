-- @@IDENTITY leaks identity values across scopes (including triggers).
INSERT INTO dbo.Orders (CustomerId) VALUES (1); SELECT @@IDENTITY AS NewId;
