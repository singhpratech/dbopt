IF (SELECT COUNT(*) FROM dbo.Orders WHERE CustomerId = 42) > 0
    PRINT 'has orders';
