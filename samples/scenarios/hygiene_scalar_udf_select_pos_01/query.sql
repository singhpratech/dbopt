-- Schema-qualified scalar UDF in the SELECT list runs once per output row (RBAR).
SELECT o.Id, dbo.fnTax(o.Total) AS Tax FROM dbo.Orders o;
