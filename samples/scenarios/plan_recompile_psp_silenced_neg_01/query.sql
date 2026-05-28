-- Version-silence: OPTION (RECOMPILE) on a parameter-driven equality predicate
-- (Region = @region) is exactly what plan.recompile_defeats_psp fires on, but
-- PSP is a 2022+ feature. Target is 2019, below the 2022 gate, so the rule must
-- stay silent.
SELECT c.CustomerId, c.Name
FROM   dbo.Customers AS c
WHERE  c.Region = @region
OPTION (RECOMPILE);
