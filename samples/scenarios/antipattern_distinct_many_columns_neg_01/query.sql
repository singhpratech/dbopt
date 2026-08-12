-- Single-table DISTINCT over a narrow projection: a legitimate de-duplication,
-- with no join to fan anything out.
SELECT DISTINCT Channel FROM dbo.Orders;
