-- Equality whose RHS is a scalar subquery. SQL Server evaluates it once and
-- seeks on the value, so ProviderId belongs in the key ahead of the range
-- column (matches the engine's own MissingIndex: EQUALITY ProviderId,
-- INEQUALITY Outstanding, INCLUDE ClaimId).
SELECT ct.ClaimId, ct.Outstanding
FROM dbo.claims_transactions ct
WHERE ct.ProviderId = (SELECT TOP 1 Id FROM dbo.providers ORDER BY Id)
  AND ct.Outstanding > 0;
