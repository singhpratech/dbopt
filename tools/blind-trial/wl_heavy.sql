SET NOCOUNT ON;
EXEC dbo.usp_EventDump '$(SINCE)';
INSERT dbo.PriceChanges (ProductID, NewPrice) SELECT $(PID) + n, 10 + n % 50 FROM dbo.vNums WHERE n <= 50;
EXEC dbo.usp_ApplyPriceChanges;
EXEC dbo.usp_OrdersByStatus 'SHIPPED';
