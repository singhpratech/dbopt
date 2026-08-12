-- A table named `Cascade` is not a referential action. The `ON DELETE CASCADE`
-- shape only means that inside a REFERENCES clause.
SET NOCOUNT ON
UPDATE Cascade SET x = 1;
