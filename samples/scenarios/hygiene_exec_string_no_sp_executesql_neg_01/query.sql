-- False-positive guard: dynamic SQL run through sp_executesql with a typed
-- parameter list instead of EXEC(@sql). The statement is parameterized, the plan
-- is reusable, and there is no injection surface, so
-- hygiene.exec_string_no_sp_executesql must stay silent.
DECLARE @sql nvarchar(max) = N'SELECT COUNT(*) FROM dbo.Orders WHERE CustomerId = @cid';
EXEC sp_executesql @sql, N'@cid int', @cid = 42;
