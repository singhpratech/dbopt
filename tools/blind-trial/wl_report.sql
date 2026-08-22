SET NOCOUNT ON;
EXEC dbo.usp_OrdersByChannelDate '$(CH)', '$(D1)', '$(D2)';
EXEC dbo.usp_ProductSearch '$(TERM)';
EXEC dbo.usp_OrdersWithTier '$(D1)', '2026-08-01';
EXEC dbo.usp_DashboardTotals;
EXEC dbo.usp_OrdersByStatus '$(ST)';
EXEC dbo.usp_DatabaseInventory;
