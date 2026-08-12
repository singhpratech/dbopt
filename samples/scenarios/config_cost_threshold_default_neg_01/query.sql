-- Raised off the 1995-era default of 5.
EXEC sp_configure 'cost threshold for parallelism', 50;
RECONFIGURE;
