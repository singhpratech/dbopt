-- Migration script turning off automatic statistics maintenance.
-- Without auto-update the histograms drift further from reality with
-- every batch insert, eventually causing stale-stats plan regressions.
ALTER DATABASE [Sales] SET AUTO_UPDATE_STATISTICS OFF;
