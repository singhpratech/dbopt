-- Orders is the PRESERVED side of a RIGHT JOIN; filtering it never demotes the join.
SELECT Customers.CustomerID, Orders.OrderID
FROM Customers RIGHT JOIN Orders ON Customers.CustomerID = Orders.CustomerID
WHERE Orders.OrderDate BETWEEN '19970101' AND '19971231';
GO
SELECT LE.TestName
FROM tSQLt.Run_LastExecution LE
RIGHT JOIN sys.dm_exec_sessions ES ON LE.SessionId = ES.session_id
WHERE ES.session_id = @@SPID;
