-- False-positive guard: a short IN-list (5 literals) is far under the 50-element
-- threshold for tempdb.large_in_clause_constant_list, so it must stay silent.
SELECT o.OrderId, o.Status
FROM   dbo.Orders AS o
WHERE  o.Status IN (1, 2, 3, 4, 5);
