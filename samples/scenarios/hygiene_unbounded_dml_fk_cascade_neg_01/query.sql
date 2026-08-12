-- `ON DELETE CASCADE` is a referential action on a constraint, not a statement.
ALTER TABLE dbo.OrderLines
ADD CONSTRAINT FK_OrderLines_Orders FOREIGN KEY (OrderId)
REFERENCES dbo.Orders (OrderId) ON DELETE CASCADE;
