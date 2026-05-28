-- ============================================================
-- Indexes (and one PERSISTED computed column) the rewrite needs.
-- Apply BEFORE running 03_optimized_query.sql.
--
-- Order key columns by selectivity. Use INCLUDE for the SELECT list
-- so the index is covering — no key lookups.
-- ============================================================
USE sqlopt_case;
GO

-- 1) Persisted, reversed-host hash for SARGable email-suffix queries.
--    `'foo@gmail.com'` reversed = `'moc.liamg@oof'`, so a prefix match on
--    the reversed string is the equivalent of a leading-wildcard match.
--    We hash for selectivity + storage.
IF COL_LENGTH('dbo.Customers', 'EmailHostReversedHash') IS NULL
BEGIN
    ALTER TABLE dbo.Customers
        ADD EmailHostReversedHash AS
            CHECKSUM(REVERSE(SUBSTRING(Email, CHARINDEX('@', Email), 254)))
            PERSISTED;
END;
GO

-- 2) Surname + status + email-host hash, covering for the report.
IF NOT EXISTS (
    SELECT 1 FROM sys.indexes
    WHERE name = 'IX_Customers_LastName_Status_HostHash' AND object_id = OBJECT_ID('dbo.Customers')
)
CREATE NONCLUSTERED INDEX IX_Customers_LastName_Status_HostHash
    ON dbo.Customers (LastName, Status, EmailHostReversedHash)
    INCLUDE (FirstName, Email, CreatedAt);
GO

-- 3) Orders: customer-window range seek.
IF NOT EXISTS (
    SELECT 1 FROM sys.indexes
    WHERE name = 'IX_Orders_CustomerId_OrderDate_Inc' AND object_id = OBJECT_ID('dbo.Orders')
)
CREATE NONCLUSTERED INDEX IX_Orders_CustomerId_OrderDate_Inc
    ON dbo.Orders (CustomerId, OrderDate)
    INCLUDE (TotalCents, Status);
GO

-- 4) OrderLines: order-id seek (heap fast lane for the inner join).
IF NOT EXISTS (
    SELECT 1 FROM sys.indexes
    WHERE name = 'IX_OrderLines_OrderId_Inc' AND object_id = OBJECT_ID('dbo.OrderLines')
)
CREATE NONCLUSTERED INDEX IX_OrderLines_OrderId_Inc
    ON dbo.OrderLines (OrderId)
    INCLUDE (ProductId, Quantity, UnitCents);
GO

-- 5) AuditLog: covering for the OUTER APPLY.
IF NOT EXISTS (
    SELECT 1 FROM sys.indexes
    WHERE name = 'IX_AuditLog_Source_LoggedAt_Inc' AND object_id = OBJECT_ID('dbo.AuditLog')
)
CREATE NONCLUSTERED INDEX IX_AuditLog_Source_LoggedAt_Inc
    ON dbo.AuditLog (Source, LoggedAt DESC)
    INCLUDE (Payload);
GO

PRINT 'indexes ready';
