-- The impersonation context is handed back before the batch ends.
EXECUTE AS LOGIN = 'HighPrivUser';
SELECT PayrollId FROM dbo.Payroll;
REVERT;
GO
