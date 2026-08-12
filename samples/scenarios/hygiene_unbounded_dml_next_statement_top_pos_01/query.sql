-- The semicolon is missing, so the scan must stop at the next statement. A
-- following SELECT's TOP does not bound this DELETE.
DELETE FROM dbo.Orders
SELECT TOP 5 OrderId FROM dbo.Users
