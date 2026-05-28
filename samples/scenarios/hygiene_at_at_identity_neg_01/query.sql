-- SCOPE_IDENTITY() returns the current-scope identity; the safe choice.
INSERT INTO dbo.Orders (CustomerId) VALUES (1); SELECT SCOPE_IDENTITY() AS NewId;
