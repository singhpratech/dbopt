-- An equality on the key names one row. An `IS NOT NULL` sitting beside it does
-- not turn a single-row update into a bulk one.
UPDATE dbo.Orders SET Status = 'closed'
WHERE OrderId = 42 AND Note IS NOT NULL;
