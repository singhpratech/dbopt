-- Enabling Optimized Locking on 2025 without Accelerated Database Recovery.
-- ADR is a hard prerequisite for the persistent-lock-versioning machinery
-- that powers Optimized Locking; turning Optimized Locking on without ADR
-- silently falls back to legacy locking and the feature does nothing.
ALTER DATABASE [Sales] SET OPTIMIZED_LOCKING = ON;
