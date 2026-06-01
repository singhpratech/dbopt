-- A covering index whose INCLUDE list is very wide. Every INCLUDE column is
-- copied into the nonclustered leaf, so the index bloats and every write that
-- touches those columns pays to maintain the copy. An honest trade-off, not a
-- free win.
CREATE NONCLUSTERED INDEX IX_Orders_CustomerId
  ON dbo.Orders (CustomerId)
  INCLUDE (OrderDate, Total, Status, ShipCity, ShipRegion, ShipCountry, Notes);
