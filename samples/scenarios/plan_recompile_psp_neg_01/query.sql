-- False-positive guard: plan.recompile_defeats_psp requires BOTH a RECOMPILE
-- hint AND a parameter-driven equality predicate (`<col> = @param`). Here the
-- equality is against a literal, not a parameter, so even on 2022+ the rule
-- must stay silent. (Single RECOMPILE also keeps option_recompile_overuse quiet.)
SELECT c.CustomerId, c.Name
FROM   dbo.Customers AS c
WHERE  c.Region = 'EMEA'
OPTION (RECOMPILE);
