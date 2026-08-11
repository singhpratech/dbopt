-- A single-row keyed UPDATE. Advising TOP (n) batching here is noise: the
-- equality predicate on a key cannot match a lock-escalating rowset.
UPDATE dbo.Orders SET Status = 'Shipped' WHERE OrderID = 42;
