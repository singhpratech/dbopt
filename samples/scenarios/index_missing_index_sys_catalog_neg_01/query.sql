-- You cannot CREATE INDEX on a system catalog view. Emitting DDL against
-- sys.indexes is a script that fails the moment anyone runs it.
IF EXISTS (SELECT * FROM sys.indexes WHERE name = 'IX_Orders_CustomerId')
    PRINT 'already there';
