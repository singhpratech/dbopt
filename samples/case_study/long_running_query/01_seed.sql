-- ============================================================
-- Seed data. Deterministic-ish; uses ABS(CHECKSUM(NEWID())) for variety.
-- Volumes are sized so the bad query is genuinely painful but the seed
-- finishes in a few minutes on a developer laptop.
-- ============================================================
USE sqlopt_case;
GO
SET NOCOUNT ON;
GO

-- ── 30 categories ─────────────────────────────────────────────
INSERT INTO dbo.ProductCategories (Name, ParentId)
SELECT TOP (30) CONCAT('Category_', ROW_NUMBER() OVER (ORDER BY (SELECT NULL))), NULL
FROM sys.all_objects a CROSS JOIN sys.all_objects b;
GO

-- ── 5,000 products ────────────────────────────────────────────
WITH n AS (
    SELECT TOP (5000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i
    FROM sys.all_objects a CROSS JOIN sys.all_objects b
)
INSERT INTO dbo.Products (CategoryId, Sku, Name, PriceCents, IsActive)
SELECT
    1 + (i % 30),
    CONCAT('SKU-', RIGHT(REPLICATE('0', 8) + CAST(i AS varchar(8)), 8)),
    CONCAT('Product ', i),
    100 + (ABS(CHECKSUM(NEWID())) % 49900),
    CASE WHEN i % 20 = 0 THEN 0 ELSE 1 END
FROM n;
GO

-- ── 50,000 customers ──────────────────────────────────────────
WITH first_names AS (
    SELECT v FROM (VALUES (N'Alice'),(N'Bob'),(N'Carol'),(N'Dan'),(N'Eve'),
                          (N'Frank'),(N'Grace'),(N'Henry'),(N'Iris'),(N'Jack')) f(v)
),
last_names AS (
    SELECT v FROM (VALUES (N'Smith'),(N'Jones'),(N'Brown'),(N'Davis'),(N'Wilson'),
                          (N'Garcia'),(N'Lopez'),(N'Khan'),(N'Lee'),(N'Patel')) l(v)
),
n AS (
    SELECT TOP (50000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i
    FROM sys.all_objects a CROSS JOIN sys.all_objects b
)
INSERT INTO dbo.Customers (FirstName, LastName, Email, Status, CreatedAt, LastSeenAt)
SELECT
    (SELECT TOP 1 v FROM first_names ORDER BY ABS(CHECKSUM(NEWID()) + n.i)),
    (SELECT TOP 1 v FROM last_names  ORDER BY ABS(CHECKSUM(NEWID()) + n.i)),
    CONCAT('user', n.i, CASE WHEN n.i % 7 = 0 THEN '@gmail.com'
                              WHEN n.i % 5 = 0 THEN '@example.com'
                              WHEN n.i % 3 = 0 THEN '@outlook.com'
                              ELSE '@yahoo.com' END),
    1 + (n.i % 6),
    DATEADD(day, -1 * (ABS(CHECKSUM(NEWID())) % 1825), SYSUTCDATETIME()),
    CASE WHEN n.i % 11 = 0 THEN NULL ELSE DATEADD(hour, -1 * (ABS(CHECKSUM(NEWID())) % 720), SYSUTCDATETIME()) END
FROM n;
GO

-- ── 2,000,000 orders ──────────────────────────────────────────
DECLARE @batch int = 0, @target int = 2000000;
WHILE @batch < @target
BEGIN
    INSERT INTO dbo.Orders (CustomerId, OrderDate, Status, TotalCents, Channel)
    SELECT TOP (100000)
        1 + (ABS(CHECKSUM(NEWID())) % 50000),
        DATEADD(minute, -1 * (ABS(CHECKSUM(NEWID())) % 525600), SYSUTCDATETIME()),
        1 + (ABS(CHECKSUM(NEWID())) % 6),
        100 + (ABS(CHECKSUM(NEWID())) % 100000),
        CASE ABS(CHECKSUM(NEWID())) % 4 WHEN 0 THEN 'web' WHEN 1 THEN 'app' WHEN 2 THEN 'pos' ELSE 'phone' END
    FROM sys.all_objects a CROSS JOIN sys.all_objects b;
    SET @batch = @batch + 100000;
    PRINT CONCAT('orders inserted: ', @batch);
END;
GO

-- ── ~6,000,000 order lines (3 per order on average) ───────────
DECLARE @batch int = 0, @target int = 6000000;
WHILE @batch < @target
BEGIN
    INSERT INTO dbo.OrderLines (OrderId, ProductId, Quantity, UnitCents)
    SELECT TOP (200000)
        1 + (ABS(CHECKSUM(NEWID())) % 2000000),
        1 + (ABS(CHECKSUM(NEWID())) % 5000),
        1 + (ABS(CHECKSUM(NEWID())) % 5),
        100 + (ABS(CHECKSUM(NEWID())) % 49900)
    FROM sys.all_objects a CROSS JOIN sys.all_objects b;
    SET @batch = @batch + 200000;
    PRINT CONCAT('order lines inserted: ', @batch);
END;
GO

PRINT 'seed complete';
