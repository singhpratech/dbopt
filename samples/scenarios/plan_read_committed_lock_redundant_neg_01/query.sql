-- False-positive guard: a plain read under the default read-committed isolation
-- with no READCOMMITTEDLOCK table hint. On 2025 with OPTIMIZED_LOCKING this lets
-- Lock-After-Qualification work as intended. There is no READCOMMITTEDLOCK hint,
-- so plan.read_committed_lock_hint_redundant_with_optimized_locking must stay
-- silent.
SELECT OrderId, CustomerId, Status
FROM dbo.Orders
WHERE Status = 'OPEN';
