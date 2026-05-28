-- Scalar UDF whose body calls a time-dependent function. Time-dependent calls
-- (GETUTCDATE) make the function non-inlineable on 2019+, so it executes
-- row-by-row for every row of any query that references it.
CREATE FUNCTION dbo.fnAgeInDays (@StartDate datetime2)
RETURNS int
AS
BEGIN
    DECLARE @result int;
    SET @result = DATEDIFF(DAY, @StartDate, GETUTCDATE());
    RETURN @result;
END
