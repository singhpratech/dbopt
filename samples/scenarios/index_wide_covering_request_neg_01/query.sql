-- A focused covering index: the INCLUDE list carries only the two columns the
-- query actually returns, so there is no write-amplification concern to flag.
CREATE NONCLUSTERED INDEX IX_Orders_CustomerId
  ON dbo.Orders (CustomerId)
  INCLUDE (Total, Status);
