-- The commas in `FETCH ... INTO @a, @b` separate output variables, not tables.
DECLARE cur CURSOR FOR SELECT Id, Total FROM dbo.Orders;
OPEN cur;
FETCH NEXT FROM cur INTO @id, @total;
