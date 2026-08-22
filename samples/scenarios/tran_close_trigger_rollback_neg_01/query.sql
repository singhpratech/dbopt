CREATE TRIGGER employee_insupd ON employee FOR INSERT, UPDATE
AS
BEGIN
    IF EXISTS (SELECT 1 FROM inserted AS i WHERE i.job_lvl > 250)
    BEGIN
        RAISERROR('The level for this job is out of range.', 16, 1);
        ROLLBACK TRANSACTION;
    END
END
