-- A password in the query text: it lands in the plan cache, in Query Store, in
-- any trace, and in source control.
SELECT * FROM OPENROWSET('SQLNCLI', 'Server=remote01;Uid=sa;Pwd=P@ssw0rd;',
                         'SELECT OrderId FROM dbo.Orders') AS r;
