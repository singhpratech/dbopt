-- False-positive guard: a single OPTION (RECOMPILE) is under the 3+ threshold
-- for plan.option_recompile_overuse, so the overuse rule must stay silent.
-- (Server pinned to 2019 so the 2022+ PSP rule is also inapplicable.)
SELECT c.CustomerId, c.Name
FROM   dbo.Customers AS c
WHERE  c.Region = 'EMEA'
OPTION (RECOMPILE);
