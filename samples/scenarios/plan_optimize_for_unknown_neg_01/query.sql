-- False-positive guard: OPTIMIZE FOR with a concrete literal value, not UNKNOWN.
-- Pinning the optimizer to a known dominant value is a deliberate, legitimate
-- choice and is not the density-only UNKNOWN anti-pattern, so
-- plan.optimize_for_unknown must stay silent.
SELECT OrderId, CustomerId, Status
FROM dbo.Orders
WHERE Status = @status
OPTION (OPTIMIZE FOR (@status = 'OPEN'));
