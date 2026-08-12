-- Nearly every production script opens with SET NOCOUNT ON. Treating the bare
-- `ON` before the verb as a referential action silenced the critical rule for
-- the statement that followed.
SET NOCOUNT ON
UPDATE dbo.T SET x = 1;
