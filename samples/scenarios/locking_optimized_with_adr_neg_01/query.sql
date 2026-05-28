-- False-positive guard: OPTIMIZED_LOCKING is enabled together with
-- ACCELERATED_DATABASE_RECOVERY = ON in the same script, which is the correct
-- ordering. Because ADR is on, maintenance.adr_required_for_optimized_locking
-- must stay silent.
ALTER DATABASE [Sales] SET ACCELERATED_DATABASE_RECOVERY = ON;
ALTER DATABASE [Sales] SET OPTIMIZED_LOCKING = ON;
