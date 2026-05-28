-- ============================================================
-- sqlopt case study: "the 10-hour sales report"
-- Schema for a small e-commerce model. Deliberately missing the
-- nonclustered indexes the pathological baseline query needs.
-- ============================================================
USE sqlopt_case;
GO

IF OBJECT_ID('dbo.OrderLines', 'U') IS NOT NULL DROP TABLE dbo.OrderLines;
IF OBJECT_ID('dbo.Orders',     'U') IS NOT NULL DROP TABLE dbo.Orders;
IF OBJECT_ID('dbo.Products',   'U') IS NOT NULL DROP TABLE dbo.Products;
IF OBJECT_ID('dbo.ProductCategories', 'U') IS NOT NULL DROP TABLE dbo.ProductCategories;
IF OBJECT_ID('dbo.Customers',  'U') IS NOT NULL DROP TABLE dbo.Customers;
IF OBJECT_ID('dbo.AuditLog',   'U') IS NOT NULL DROP TABLE dbo.AuditLog;
IF OBJECT_ID('dbo.fnFullName', 'FN') IS NOT NULL DROP FUNCTION dbo.fnFullName;
GO

CREATE TABLE dbo.Customers (
    CustomerId       int           IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    FirstName        nvarchar(60)  NOT NULL,
    LastName         nvarchar(60)  NOT NULL,
    Email            varchar(254)  NOT NULL,           -- varchar on purpose
    Status           tinyint       NOT NULL,           -- 1..6
    CreatedAt        datetime2(3)  NOT NULL DEFAULT SYSUTCDATETIME(),
    LastSeenAt       datetime2(3)  NULL
);

CREATE TABLE dbo.ProductCategories (
    CategoryId   int          IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    Name         nvarchar(60) NOT NULL,
    ParentId     int          NULL
);

CREATE TABLE dbo.Products (
    ProductId     int           IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    CategoryId    int           NOT NULL REFERENCES dbo.ProductCategories(CategoryId),
    Sku           varchar(40)   NOT NULL,
    Name          nvarchar(120) NOT NULL,
    PriceCents    int           NOT NULL,
    IsActive      bit           NOT NULL DEFAULT (1)
);

CREATE TABLE dbo.Orders (
    OrderId      bigint        IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    CustomerId   int           NOT NULL REFERENCES dbo.Customers(CustomerId),
    OrderDate    datetime2(3)  NOT NULL,
    Status       tinyint       NOT NULL,                 -- 1..6
    TotalCents   bigint        NOT NULL,
    Channel      varchar(20)   NOT NULL                  -- 'web' | 'app' | 'pos' | 'phone'
);

CREATE TABLE dbo.OrderLines (
    OrderLineId  bigint        IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    OrderId      bigint        NOT NULL REFERENCES dbo.Orders(OrderId),
    ProductId    int           NOT NULL REFERENCES dbo.Products(ProductId),
    Quantity     int           NOT NULL,
    UnitCents    int           NOT NULL
);

-- AuditLog deliberately uses a legacy LOB type so the analyzer flags it.
CREATE TABLE dbo.AuditLog (
    AuditId   bigint  IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    Source    varchar(60) NOT NULL,
    Payload   text        NULL,                     -- deprecated; modern is varchar(max)
    LoggedAt  datetime2(3) NOT NULL DEFAULT SYSUTCDATETIME()
);
GO

CREATE FUNCTION dbo.fnFullName (@first nvarchar(60), @last nvarchar(60))
RETURNS nvarchar(140)
WITH SCHEMABINDING
AS
BEGIN
    -- Intentionally scalar so we can exercise the scalar-UDF-in-predicate rule.
    RETURN LTRIM(RTRIM(@first)) + N' ' + LTRIM(RTRIM(@last));
END;
GO

PRINT 'schema ready';
