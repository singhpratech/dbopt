-- Maintenance script disabling automatic statistics creation database-wide.
-- This is almost always a mistake: the optimizer relies on auto-created
-- single-column stats to estimate selectivity for predicates that aren't
-- already covered by an index.
ALTER DATABASE [Sales] SET AUTO_CREATE_STATISTICS OFF;
