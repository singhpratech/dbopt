-- An INSERT column list is not a function call. `Orders` is an unqualified
-- table reference and must still draw schema-prefix advice.
INSERT INTO Orders (OrderId, Total) VALUES (1, 9.99);
