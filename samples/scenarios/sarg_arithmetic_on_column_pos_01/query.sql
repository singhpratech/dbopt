-- Arithmetic on the indexed column side defeats the index seek (non-SARGable).
SELECT * FROM dbo.Orders WHERE Qty + 1 = 5;
