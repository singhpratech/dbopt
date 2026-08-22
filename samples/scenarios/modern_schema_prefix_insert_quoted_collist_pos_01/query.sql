-- A quoted INSERT target followed by a column list on the next line is an
-- unqualified table reference (not a function call); it must still be reported.
INSERT INTO "Orders"
("OrderID","CustomerID","OrderDate")
VALUES (10248, 'VINET', '1996-07-04');
