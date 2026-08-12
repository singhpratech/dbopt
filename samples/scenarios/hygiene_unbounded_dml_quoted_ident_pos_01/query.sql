-- A double-quoted identifier reaches the rules as a bare word: the tokenizer
-- emits the quotes separately. Read as the MERGE keyword, it silenced the
-- genuinely unbounded UPDATE on the next line.
UPDATE dbo.Flags SET "Merge" = 1 WHERE Id = 3
UPDATE dbo.T SET x = 2
