-- Version-silence: a scalar UDF whose body calls a time-dependent function
-- (GETDATE) is exactly what plan.scalar_udf_block_inlining flags as a blocker.
-- Scalar UDF inlining is a 2019+ feature; target is 2016, below the 2019 gate,
-- so the rule must stay silent.
CREATE FUNCTION dbo.AgeInDays(@since DATETIME2)
RETURNS INT
AS
BEGIN
    RETURN DATEDIFF(DAY, @since, GETDATE());
END;
