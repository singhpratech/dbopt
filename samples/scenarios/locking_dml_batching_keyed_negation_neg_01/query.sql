-- `<>` reaches the rule as two adjacent punctuation tokens, not one. Read as a
-- lone `<` it looked like a range scan; beside an equality on the key it is a
-- secondary condition on a row already identified.
UPDATE dbo.Orders SET Status = 'closed'
WHERE OrderId = 42 AND Status <> 'closed';
