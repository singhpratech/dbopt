-- `[Top]` is a column, not the row limiter. Reading it as one bounded a
-- statement that rewrites every row.
UPDATE dbo.Config SET [Top] = 10;
