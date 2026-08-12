-- `[Select]` is a column, not a statement head. Ending the forward scan on it
-- meant this bounded UPDATE never reached its own WHERE clause and was reported
-- as rewriting every row — a false positive at critical severity.
UPDATE dbo.Flags SET [Select] = 1, [Go] = 2 WHERE Id = 3;
