-- False-positive guard: a short OR chain with only two OR operators (three
-- predicates) sits under the 3-OR threshold for sarg.or_chain, so the rule
-- must stay silent.
SELECT t.TicketId, t.Priority
FROM   dbo.Tickets AS t
WHERE  t.Priority = 'High'
   OR  t.Priority = 'Critical';
