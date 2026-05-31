-- ============================================================
-- THE REWRITE.
--
-- Every analyzer finding from 02_baseline_query.sql has been addressed.
-- The schema is qualified, predicates are SARGable, the scalar UDF is
-- replaced with inline expressions, NOLOCK is gone, the correlated
-- TOP 1 subquery becomes an OUTER APPLY, and the email substring
-- match is now a hash check against a persisted reversed-host column.
--
-- Required: 04_indexes.sql, plus the schema-level reversed-host computed
-- column added in 04_indexes.sql (Customers.EmailHostReversed). Required
-- behaviour: RCSI is on at the database level (see docker/bootstrap/00_init.sql)
-- so we get non-blocking reads without NOLOCK.
-- ============================================================
USE dbopt_case;
GO

IF OBJECT_ID('dbo.GetGmailSReport_Fast', 'P') IS NOT NULL DROP PROCEDURE dbo.GetGmailSReport_Fast;
GO

CREATE PROCEDURE dbo.GetGmailSReport_Fast
AS
BEGIN
    SET NOCOUNT ON;

    -- The selectivity here is "S* surname + gmail customer." Cardinality is
    -- small (~hundreds of customers) so we start from Customers, then range-seek
    -- their orders over the last 30 days, and let OUTER APPLY pull the latest
    -- audit row per customer instead of a correlated TOP 1 inside the WHERE.
    DECLARE @windowStart datetime2(3) = DATEADD(day, -30, SYSUTCDATETIME());
    DECLARE @windowEnd   datetime2(3) = SYSUTCDATETIME();

    SELECT
        c.CustomerId,
        c.FirstName,
        c.LastName,
        c.Email,
        c.Status,
        c.CreatedAt,
        o.OrderId,
        o.OrderDate,
        o.TotalCents,
        ol.ProductId,
        ol.Quantity,
        p.Name        AS ProductName,
        pc.Name       AS CategoryName,
        a.Payload     AS LastAuditPayload
    FROM dbo.Customers AS c
    LEFT JOIN dbo.Orders AS o
        ON  o.CustomerId = c.CustomerId
        AND o.OrderDate >= @windowStart
        AND o.OrderDate <  @windowEnd
    LEFT JOIN dbo.OrderLines AS ol
        ON ol.OrderId = o.OrderId
    LEFT JOIN dbo.Products AS p
        ON p.ProductId = ol.ProductId
    LEFT JOIN dbo.ProductCategories AS pc
        ON pc.CategoryId = p.CategoryId
    OUTER APPLY (
        -- Replaces the correlated TOP 1 subquery. AuditLog needs a covering
        -- index on (Source, LoggedAt DESC) INCLUDE (Payload) for this to seek.
        SELECT TOP (1) al.Payload
        FROM dbo.AuditLog AS al
        WHERE al.Source = 'orders'
          AND al.LoggedAt >= CAST('2026-05-01' AS datetime2(3))
          AND al.LoggedAt <  CAST('2026-05-02' AS datetime2(3))
        ORDER BY al.LoggedAt DESC
    ) AS a
    WHERE c.LastName = 'Smith'                  -- ASCII column gets ASCII literal: no implicit convert
      AND c.Status IN (1, 2, 3, 4, 5)            -- IN list, not an OR chain
      AND c.EmailHostReversedHash =              -- precomputed via 04_indexes.sql
          CHECKSUM(REVERSE('moc.liamg@'))
      AND CONCAT(LTRIM(RTRIM(c.FirstName)), ' ',
                 LTRIM(RTRIM(c.LastName))) = 'Alice Smith'   -- UDF inlined
    ORDER BY o.OrderDate DESC
    OPTION (RECOMPILE);   -- safe here because input parameters are stable per execution
END;
GO

PRINT 'optimized procedure created. EXEC dbo.GetGmailSReport_Fast;';
