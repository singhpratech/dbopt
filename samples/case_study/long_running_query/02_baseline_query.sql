-- ============================================================
-- THE BASELINE QUERY — the "10-hour sales report".
--
-- Business intent: for every gmail.com customer whose last name
-- starts with the letter S, surface their last 30-day order count,
-- total revenue, and the friendliest product category. The team
-- shipped this with NOLOCK because "production was slow" — which
-- of course did not actually make it fast.
--
-- The analyzer should flag, at minimum:
--   hygiene.select_star, hygiene.nolock, hygiene.cursor (in 4a)
--   hygiene.top_without_order_by, hygiene.unbounded_dml (in 4a),
--   sarg.function_on_column, sarg.leading_wildcard,
--   sarg.implicit_convert_unicode, sarg.or_chain,
--   sarg.scalar_udf_in_predicate, modern.missing_schema_prefix,
--   modern.missing_set_nocount, deprecated.lob_legacy_types
--   (declared on AuditLog via 00_schema.sql)
-- ============================================================
USE dbopt_case;
GO

IF OBJECT_ID('dbo.GetGmailSReport', 'P') IS NOT NULL DROP PROCEDURE dbo.GetGmailSReport;
GO

CREATE PROCEDURE dbo.GetGmailSReport          -- intentionally missing SET NOCOUNT ON
AS
BEGIN
    -- SELECT * is intentional here; the report consumer "needs everything".
    SELECT *
    FROM Customers c WITH (NOLOCK)             -- missing schema prefix + NOLOCK
    LEFT JOIN Orders o WITH (NOLOCK)
        ON o.CustomerId = c.CustomerId
       AND o.OrderDate >= DATEADD(day, -30, GETDATE())
    LEFT JOIN OrderLines ol WITH (NOLOCK)
        ON ol.OrderId = o.OrderId
    LEFT JOIN Products p
        ON p.ProductId = ol.ProductId
    LEFT JOIN ProductCategories pc
        ON pc.CategoryId = p.CategoryId
    WHERE UPPER(c.LastName) = 'SMITH'           -- function on indexed column
      AND c.Email LIKE '%@gmail.com'           -- leading wildcard
      AND dbo.fnFullName(c.FirstName, c.LastName) = N'Alice Smith'  -- scalar UDF + implicit convert
      AND c.LastName = N'Smith'                -- N'…' against varchar -> implicit convert
      AND (
            c.Status = 1
         OR c.Status = 2
         OR c.Status = 3
         OR c.Status = 4
         OR c.Status = 5
          )
      AND (
            SELECT TOP 1 al.Payload              -- correlated TOP 1 subquery; classic
            FROM AuditLog al WITH (NOLOCK)
            WHERE al.Source = 'orders'
              AND CAST(al.LoggedAt AS date) = '2026-05-01'
            ORDER BY al.AuditId DESC
          ) IS NOT NULL
    ORDER BY o.OrderDate DESC;
END;
GO

PRINT 'baseline procedure created. EXEC dbo.GetGmailSReport;';
