Classic non-SARGable predicate: `UPPER(LastName) = 'SMITH'` forces the optimizer to evaluate the function for every row, defeating any index seek on `LastName`. Should fire `sarg.function_on_column`.
