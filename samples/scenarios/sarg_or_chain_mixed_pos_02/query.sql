-- A parameter guard does not excuse a real OR chain on the column behind it:
-- the four column predicates are still a chain, and the LIKE still leads with
-- a wildcard.
SELECT TicketId
FROM dbo.Tickets
WHERE @all = 1
   OR Priority = 'High'
   OR Priority = 'Critical'
   OR Priority = 'Blocker'
   OR Subject LIKE '%outage%';
