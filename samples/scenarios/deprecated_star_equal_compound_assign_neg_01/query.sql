-- `*=` here is the compound multiply-assign operator (2008+), not the removed
-- outer-join syntax. It tokenizes identically.
DECLARE @MinutesBack int = 5;
SET @MinutesBack *= -1;
