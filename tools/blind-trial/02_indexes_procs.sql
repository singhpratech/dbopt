USE BlindTrial;
GO
-- ===== Indexes =====
-- D2: exact duplicate index pair
CREATE INDEX IX_Customers_Email ON dbo.Customers (Email);
CREATE INDEX IX_Customers_Email_Dup ON dbo.Customers (Email);
-- D3: genuinely unused index (workload never touches Phone, never writes Customers)
CREATE INDEX IX_Customers_Phone ON dbo.Customers (Phone);
-- D11: key-only index on skewed Status => lookups; sniffing victim
CREATE INDEX IX_Orders_Status ON dbo.Orders (Status);
-- D17: low fill-factor index
CREATE INDEX IX_OrderLines_ProductID ON dbo.OrderLines (ProductID) INCLUDE (Qty, UnitPrice) WITH (FILLFACTOR = 50);
-- D16: stats that never auto-update, then load more rows (below)
CREATE INDEX IX_Events_OccurredAt ON dbo.Events (OccurredAt) INCLUDE (EventType) WITH (STATISTICS_NORECOMPUTE = ON);
-- B3: look-alike pair -- same key, DIFFERENT include sets, each serving its own query
CREATE INDEX IX_Products_CategoryID_Price ON dbo.Products (CategoryID) INCLUDE (Price);
CREATE INDEX IX_Products_CategoryID_Stock ON dbo.Products (CategoryID) INCLUDE (StockQty, SKU);
GO
-- D16: load 600k rows AFTER stats built; NORECOMPUTE => stats stay at 400k
INSERT dbo.Events (CustomerID, EventType, OccurredAt, Payload)
SELECT 1 + (n * 48271 % 200000),
       CHOOSE(1 + n % 8, 'PAGE_VIEW', 'PAGE_VIEW', 'PAGE_VIEW', 'ADD_TO_CART', 'CHECKOUT', 'LOGIN', 'SEARCH', 'LOGOUT'),
       DATEADD(SECOND, (n * 7919) % 31536000, '2025-08-01'),
       N'{"session":"' + CAST(NEWID() AS nvarchar(36)) + N'","ref":"' + CAST(n % 997 AS nvarchar(5)) + N'"}'
FROM dbo.vNums WHERE n <= 600000;
GO
-- ===== Functions =====
-- D10: scalar UDF (data access) used per row
CREATE FUNCTION dbo.fn_CustomerTierLabel (@CustomerID int) RETURNS varchar(10) AS
BEGIN
  DECLARE @t tinyint = (SELECT Tier FROM dbo.Customers WHERE CustomerID = @CustomerID);
  RETURN CASE @t WHEN 1 THEN 'BRONZE' WHEN 2 THEN 'SILVER' WHEN 3 THEN 'GOLD' ELSE 'PLATINUM' END;
END
GO
-- ===== Procs =====
-- D1: missing index (Channel, OrderDate) -- clustered scan of 1M rows
CREATE PROC dbo.usp_OrdersByChannelDate @Channel varchar(10), @From datetime2(0), @To datetime2(0) AS
  SELECT OrderID, CustomerID, OrderDate, TotalAmount FROM dbo.Orders
  WHERE Channel = @Channel AND OrderDate >= @From AND OrderDate < @To;
GO
-- D7: function on column => non-sargable, scans IX_Customers_Email
CREATE PROC dbo.usp_CustomerByEmail @Email varchar(200) AS
  SELECT CustomerID, Email, Region, Tier FROM dbo.Customers WHERE LOWER(Email) = LOWER(@Email);
GO
-- D8: leading wildcard
CREATE PROC dbo.usp_ProductSearch @Term varchar(50) AS
  SELECT TOP (50) ProductID, SKU, Name, Price FROM dbo.Products WHERE Name LIKE '%' + @Term + '%';
GO
-- D9: implicit conversion (nvarchar param vs varchar column)
CREATE PROC dbo.usp_ShipmentByTracking @Code nvarchar(40) AS
  SELECT ShipmentID, OrderID, Carrier, ShippedAt FROM dbo.Shipments WHERE TrackingCode = @Code;
GO
-- B5: correct type -- must NOT be flagged for implicit conversion
CREATE PROC dbo.usp_ShipmentByTrackingTyped @Code varchar(40) AS
  SELECT ShipmentID, OrderID, Carrier, ShippedAt FROM dbo.Shipments WHERE TrackingCode = @Code;
GO
-- D10: scalar UDF per row over many rows
CREATE PROC dbo.usp_OrdersWithTier @From datetime2(0), @To datetime2(0) AS
  SELECT OrderID, CustomerID, dbo.fn_CustomerTierLabel(CustomerID) AS TierLabel, TotalAmount
  FROM dbo.Orders WHERE OrderID BETWEEN 1 AND 50000 AND OrderDate >= @From AND OrderDate < @To;
GO
-- D11: parameter sniffing (Status heavily skewed; key-only index => lookups)
CREATE PROC dbo.usp_OrdersByStatus @Status varchar(20) AS
  SELECT OrderID, CustomerID, OrderDate, TotalAmount FROM dbo.Orders WHERE Status = @Status;
GO
-- D12: join on un-indexed FK column => Orders scan
CREATE PROC dbo.usp_CustomerOrderSummary @CustomerID int AS
  SELECT c.CustomerID, c.Email, COUNT(o.OrderID) AS Orders, SUM(o.TotalAmount) AS Spend
  FROM dbo.Customers c LEFT JOIN dbo.Orders o ON o.CustomerID = c.CustomerID
  WHERE c.CustomerID = @CustomerID GROUP BY c.CustomerID, c.Email;
GO
-- D13: SELECT * + ORDER BY on a non-indexed column over a big range => large sort
CREATE PROC dbo.usp_EventDump @Since datetime2(0) AS
  SELECT * FROM dbo.Events WHERE OccurredAt >= @Since ORDER BY Payload DESC;
GO
-- D14: NOLOCK
CREATE PROC dbo.usp_DashboardTotals AS
  SELECT o.Status, COUNT(*) AS Orders, SUM(ol.Qty * ol.UnitPrice) AS Revenue
  FROM dbo.Orders o WITH (NOLOCK) JOIN dbo.OrderLines ol WITH (NOLOCK) ON ol.OrderID = o.OrderID
  WHERE o.OrderID BETWEEN 1 AND 20000 GROUP BY o.Status;
GO
-- D15: cursor doing row-by-row DML
CREATE PROC dbo.usp_ApplyPriceChanges AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @id int, @pid int, @p decimal(10,2);
  DECLARE cur CURSOR FOR SELECT ChangeID, ProductID, NewPrice FROM dbo.PriceChanges WHERE Applied = 0;
  OPEN cur; FETCH NEXT FROM cur INTO @id, @pid, @p;
  WHILE @@FETCH_STATUS = 0
  BEGIN
    UPDATE dbo.Products SET Price = @p WHERE ProductID = @pid;
    UPDATE dbo.PriceChanges SET Applied = 1 WHERE ChangeID = @id;
    FETCH NEXT FROM cur INTO @id, @pid, @p;
  END
  CLOSE cur; DEALLOCATE cur;
END
GO
-- D18c: deprecated SET ROWCOUNT
CREATE PROC dbo.usp_TopProductsInCategory @CategoryID int AS
BEGIN
  SET ROWCOUNT 10;
  SELECT ProductID, Price FROM dbo.Products WHERE CategoryID = @CategoryID ORDER BY Price DESC;
  SET ROWCOUNT 0;
END
GO
-- D4: heap writes (insert + growing update => forwarded records)
CREATE PROC dbo.usp_LogOrderAction @OrderID int, @Action varchar(20) AS
BEGIN
  SET NOCOUNT ON;
  INSERT dbo.AuditLog (OrderID, Action, Actor, Note) VALUES (@OrderID, @Action, 'api', 'x');
  UPDATE dbo.AuditLog SET Note = REPLICATE('audit detail ', 20) WHERE AuditID = SCOPE_IDENTITY();
END
GO
-- Normal workload procs (serve B3 both indexes; use OrderLines/Orders/Shipments indexes)
CREATE PROC dbo.usp_CategoryPriceStats @CategoryID int AS
  SELECT COUNT(*) AS N, AVG(Price) AS AvgPrice, MAX(Price) AS MaxPrice FROM dbo.Products WHERE CategoryID = @CategoryID;
GO
CREATE PROC dbo.usp_CategoryLowStock @CategoryID int AS
  SELECT SKU, StockQty FROM dbo.Products WHERE CategoryID = @CategoryID AND StockQty < 5;
GO
CREATE PROC dbo.usp_OrderDetail @OrderID int AS
  SELECT o.OrderID, o.OrderDate, o.Status, ol.ProductID, ol.Qty, ol.UnitPrice
  FROM dbo.Orders o JOIN dbo.OrderLines ol ON ol.OrderID = o.OrderID WHERE o.OrderID = @OrderID;
GO
CREATE PROC dbo.usp_ProductSales @ProductID int AS
  SELECT SUM(Qty) AS Units, SUM(Qty * UnitPrice) AS Revenue FROM dbo.OrderLines WHERE ProductID = @ProductID;
GO
CREATE PROC dbo.usp_CustomerEvents @CustomerID int AS
  SELECT TOP (100) EventType, OccurredAt FROM dbo.Events WHERE CustomerID = @CustomerID ORDER BY OccurredAt DESC;
GO
-- B1: catalog-only cursor (tiny, read-only, no DML) -- must NOT be flagged as RBAR DML
CREATE PROC dbo.usp_DatabaseInventory AS
BEGIN
  SET NOCOUNT ON;
  DECLARE @name sysname, @out nvarchar(max) = N'';
  DECLARE dbs CURSOR LOCAL FAST_FORWARD FOR SELECT name FROM sys.databases WHERE state = 0;
  OPEN dbs; FETCH NEXT FROM dbs INTO @name;
  WHILE @@FETCH_STATUS = 0 BEGIN SET @out += @name + N';'; FETCH NEXT FROM dbs INTO @name; END
  CLOSE dbs; DEALLOCATE dbs;
  SELECT @out AS Databases;
END
GO
-- B4: lookup on a 20-row heap -- must NOT get a missing-index / heap finding
CREATE PROC dbo.usp_CategoryByName @Name varchar(60) AS
  SELECT CategoryID, Margin FROM dbo.Categories WHERE Name = @Name;
GO
-- D4 (cont.): grows existing heap rows in place => forwarded records
CREATE PROC dbo.usp_AnnotateAudit @AuditID int, @Note varchar(500) AS
  UPDATE dbo.AuditLog SET Note = @Note WHERE AuditID = @AuditID;
GO
