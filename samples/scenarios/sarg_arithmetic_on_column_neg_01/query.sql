-- Math kept on the constant side; the column stays bare and remains SARGable.
SELECT * FROM dbo.Orders WHERE Qty = 5 - 1;
