-- `TOP n` between SELECT and `*` does not stop this being SELECT *.
SELECT TOP 5 * FROM dbo.Orders ORDER BY OrderId;
