-- BlindTrial: schema + data (run in master)
SET NOCOUNT ON;
IF DB_ID('BlindTrial') IS NOT NULL
BEGIN
  ALTER DATABASE BlindTrial SET SINGLE_USER WITH ROLLBACK IMMEDIATE;
  DROP DATABASE BlindTrial;
END
CREATE DATABASE BlindTrial;
ALTER DATABASE BlindTrial SET RECOVERY SIMPLE;
ALTER DATABASE BlindTrial SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE, QUERY_CAPTURE_MODE = ALL, INTERVAL_LENGTH_MINUTES = 1, DATA_FLUSH_INTERVAL_SECONDS = 60);
GO
USE BlindTrial;
GO
-- numbers helper (view)
CREATE VIEW dbo.vNums AS
SELECT TOP (4000000) n = ROW_NUMBER() OVER (ORDER BY (SELECT NULL))
FROM sys.all_columns a CROSS JOIN sys.all_columns b CROSS JOIN sys.all_columns c;
GO
-- B2: small lookup HEAP (20 rows) -- legitimately fine
CREATE TABLE dbo.Categories (CategoryID int NOT NULL, Name varchar(60) NOT NULL, Margin decimal(5,2) NOT NULL);
INSERT dbo.Categories SELECT n, 'Category ' + CAST(n AS varchar(3)), 10 + n FROM dbo.vNums WHERE n <= 20;

CREATE TABLE dbo.Customers (
  CustomerID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_Customers PRIMARY KEY CLUSTERED,
  Email varchar(200) NOT NULL, Phone varchar(30) NULL, Region char(2) NOT NULL,
  Tier tinyint NOT NULL, CreatedAt datetime2(0) NOT NULL, LifetimeValue decimal(12,2) NOT NULL DEFAULT 0);
INSERT dbo.Customers (Email, Phone, Region, Tier, CreatedAt)
SELECT 'user' + CAST(n AS varchar(10)) + '@example' + CAST(n % 7 AS varchar(2)) + '.com',
       '+1' + RIGHT('0000000000' + CAST(ABS(CHECKSUM(NEWID())) % 10000000000 AS varchar(12)), 10),
       CHAR(65 + n % 26) + CHAR(65 + (n / 26) % 26),
       1 + n % 4,
       DATEADD(SECOND, -(n * 37 % 94608000), '2026-08-01')
FROM dbo.vNums WHERE n <= 200000;

CREATE TABLE dbo.Products (
  ProductID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_Products PRIMARY KEY CLUSTERED,
  SKU varchar(40) NOT NULL, Name varchar(120) NOT NULL, CategoryID int NOT NULL,
  Price decimal(10,2) NOT NULL, StockQty int NOT NULL,
  Description ntext NULL);   -- D18a deprecated type
INSERT dbo.Products (SKU, Name, CategoryID, Price, StockQty, Description)
SELECT 'SKU-' + RIGHT('00000000' + CAST(n AS varchar(10)), 8),
       CHOOSE(1 + n % 6, 'Widget', 'Gadget', 'Gizmo', 'Doohickey', 'Thingamajig', 'Contraption') + ' ' + CAST(n AS varchar(10)) + CHOOSE(1 + n % 5, ' Pro', ' Lite', ' Max', ' Mini', ' Plus'),
       1 + n % 20, 1 + (n * 7919 % 50000) / 100.0, n % 500,
       CASE WHEN n % 10 = 0 THEN N'Long description text ' + CAST(n AS nvarchar(10)) END
FROM dbo.vNums WHERE n <= 50000;

CREATE TABLE dbo.Orders (
  OrderID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_Orders PRIMARY KEY CLUSTERED,
  CustomerID int NOT NULL, OrderDate datetime2(0) NOT NULL,
  Status varchar(20) NOT NULL, Channel varchar(10) NOT NULL,
  TotalAmount decimal(12,2) NOT NULL, Notes nvarchar(200) NULL);
INSERT dbo.Orders (CustomerID, OrderDate, Status, Channel, TotalAmount, Notes)
SELECT 1 + (n * 48271 % 200000),
       DATEADD(SECOND, n * 31 % 63072000, '2024-08-01'),
       CASE WHEN n % 1000 = 0 THEN 'CANCELLED' WHEN n % 1000 < 20 THEN 'RETURNED' WHEN n % 1000 < 80 THEN 'PENDING' ELSE 'SHIPPED' END,  -- skew: 0.1% CANCELLED, 92% SHIPPED
       CHOOSE(1 + n % 4, 'WEB', 'MOBILE', 'STORE', 'PARTNER'),
       5 + (n * 104729 % 100000) / 100.0,
       CASE WHEN n % 50 = 0 THEN N'gift wrap requested' END
FROM dbo.vNums WHERE n <= 1000000;
-- D12: FK with NO supporting index on Orders.CustomerID
ALTER TABLE dbo.Orders ADD CONSTRAINT FK_Orders_Customers FOREIGN KEY (CustomerID) REFERENCES dbo.Customers(CustomerID);

CREATE TABLE dbo.OrderLines (
  OrderLineID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_OrderLines PRIMARY KEY CLUSTERED,
  OrderID int NOT NULL, ProductID int NOT NULL, Qty smallint NOT NULL,
  UnitPrice decimal(10,2) NOT NULL, Discount decimal(5,2) NOT NULL);
INSERT dbo.OrderLines (OrderID, ProductID, Qty, UnitPrice, Discount)
SELECT 1 + ((n - 1) / 2), 1 + (n * 7919 % 50000), 1 + n % 5, 1 + (n * 31 % 20000) / 100.0, (n % 4) * 2.5
FROM dbo.vNums WHERE n <= 2000000;
ALTER TABLE dbo.OrderLines ADD CONSTRAINT FK_OrderLines_Orders FOREIGN KEY (OrderID) REFERENCES dbo.Orders(OrderID);
CREATE INDEX IX_OrderLines_OrderID ON dbo.OrderLines (OrderID) INCLUDE (ProductID, Qty, UnitPrice, Discount);

-- D5: no PRIMARY KEY (non-unique clustered index only)
CREATE TABLE dbo.Shipments (
  ShipmentID int IDENTITY(1,1) NOT NULL, OrderID int NOT NULL,
  TrackingCode varchar(40) NOT NULL, Carrier varchar(20) NOT NULL,
  ShippedAt datetime2(0) NULL, Notes text NULL);   -- D18b deprecated type
CREATE CLUSTERED INDEX CIX_Shipments ON dbo.Shipments (ShipmentID);
INSERT dbo.Shipments (OrderID, TrackingCode, Carrier, ShippedAt, Notes)
SELECT n * 3, 'TRK' + RIGHT('000000000000' + CAST(n * 7 AS varchar(12)), 12) + CHAR(65 + n % 26),
       CHOOSE(1 + n % 3, 'UPS', 'FEDEX', 'DHL'),
       DATEADD(SECOND, n * 93 % 63072000, '2024-08-02'),
       CASE WHEN n % 100 = 0 THEN 'left at door' END
FROM dbo.vNums WHERE n <= 300000;
CREATE INDEX IX_Shipments_TrackingCode ON dbo.Shipments (TrackingCode);

-- D6: wide clustered key (GUID, random inserts => fragmentation)
CREATE TABLE dbo.Events (
  EventGuid uniqueidentifier NOT NULL CONSTRAINT PK_Events PRIMARY KEY CLUSTERED DEFAULT NEWID(),
  CustomerID int NOT NULL, EventType varchar(30) NOT NULL,
  OccurredAt datetime2(0) NOT NULL, Payload nvarchar(400) NOT NULL);
INSERT dbo.Events (CustomerID, EventType, OccurredAt, Payload)
SELECT 1 + (n * 48271 % 200000),
       CHOOSE(1 + n % 8, 'PAGE_VIEW', 'PAGE_VIEW', 'PAGE_VIEW', 'ADD_TO_CART', 'CHECKOUT', 'LOGIN', 'SEARCH', 'LOGOUT'),
       DATEADD(SECOND, n * 17 % 31536000, '2025-08-01'),
       N'{"session":"' + CAST(NEWID() AS nvarchar(36)) + N'","ref":"' + CAST(n % 997 AS nvarchar(5)) + N'"}'
FROM dbo.vNums WHERE n <= 400000;
CREATE INDEX IX_Events_CustomerID ON dbo.Events (CustomerID);

-- D4: HEAP with real writes (has a nonclustered PK, so distinct from "no PK")
CREATE TABLE dbo.AuditLog (
  AuditID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_AuditLog PRIMARY KEY NONCLUSTERED,
  OrderID int NOT NULL, Action varchar(20) NOT NULL, Actor varchar(50) NOT NULL,
  LoggedAt datetime2(3) NOT NULL DEFAULT SYSDATETIME(), Note varchar(500) NULL);
INSERT dbo.AuditLog (OrderID, Action, Actor, LoggedAt, Note)
SELECT 1 + n % 1000000, CHOOSE(1 + n % 3, 'CREATE', 'UPDATE', 'SHIP'), 'svc' + CAST(n % 9 AS varchar(1)),
       DATEADD(SECOND, n, '2026-01-01'), 'init'
FROM dbo.vNums WHERE n <= 100000;

CREATE TABLE dbo.PriceChanges (
  ChangeID int IDENTITY(1,1) NOT NULL CONSTRAINT PK_PriceChanges PRIMARY KEY CLUSTERED,
  ProductID int NOT NULL, NewPrice decimal(10,2) NOT NULL, Applied bit NOT NULL DEFAULT 0);
GO
