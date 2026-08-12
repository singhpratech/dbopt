-- Both operands are variables: no column, no index, nothing to rewrite.
SELECT Id FROM dbo.Items WHERE Id > 0;
IF UPPER(@@SERVERNAME) <> UPPER(@ServerName)
    PRINT 'different server';
