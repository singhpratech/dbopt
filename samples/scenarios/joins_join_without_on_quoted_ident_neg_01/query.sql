-- With QUOTED_IDENTIFIER ON, "Order Details" is a NAME. Reading the word Order
-- inside it as the ORDER BY keyword truncated the clause scan, so this ordinary
-- join was reported as having no ON clause.
SELECT p.ProductID
FROM Products AS p
INNER JOIN "Order Details" ON p.ProductID = "Order Details".ProductID;
