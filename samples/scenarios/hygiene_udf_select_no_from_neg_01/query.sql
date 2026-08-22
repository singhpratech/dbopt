-- FROM-less assignment selects evaluate the function exactly once.
SELECT @TestProcName = tSQLt.Private_GetCleanObjectName(@TestName);
SELECT @Formatter = COALESCE(@TestResultFormatter, tSQLt.GetTestResultFormatter());
