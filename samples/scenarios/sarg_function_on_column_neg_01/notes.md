`DATEADD` and `GETDATE` are used here but only against literals — the indexed column `OrderDate` is left bare. This is SARGable and should NOT fire `sarg.function_on_column`.
