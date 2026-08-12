-- A double-quoted identifier is still an unqualified table reference. Before the
-- tokenizer understood them, this was invisible.
SELECT OrderId FROM "Order Details";
