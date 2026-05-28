-- Query that forces the READCOMMITTEDLOCK table hint. With OPTIMIZED_LOCKING on
-- (2025+) this hint defeats Lock-After-Qualification, re-introducing the locking
-- overhead the engine would otherwise avoid.
SELECT AccountId, Balance
FROM dbo.Accounts WITH (READCOMMITTEDLOCK)
WHERE AccountId = @id;
