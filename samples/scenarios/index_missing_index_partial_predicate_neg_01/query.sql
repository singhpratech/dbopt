-- One conjunct (LIKE with a leading wildcard) cannot be modelled as a seek.
-- Building a key from the surviving ProviderId = 1 alone would recommend an
-- index shaped for a different query, so the rule must stay silent rather
-- than emit a partial-predicate key.
SELECT ct.ClaimId, ct.Outstanding
FROM dbo.claims_transactions ct
WHERE ct.ProviderId = 1
  AND ct.Note LIKE '%overdue%';
