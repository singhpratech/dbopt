-- `>=` arrives from the lexer as two tokens. Reading the `=` as the value
-- rejected every `col >= 'x'` conjunct and, with it, the whole statement —
-- the equality column AND the range column were silently dropped.
SELECT ct.PatientId, ct.Amount
FROM   dbo.claims_transactions AS ct
WHERE  ct.Type = 'CHARGE' AND ct.FromDate >= '2021-01-01';
