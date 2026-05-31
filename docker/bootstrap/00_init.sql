-- Run with: sqlcmd -S localhost,14333 -U sa -P "$SA_PASSWORD" -C -i docker/bootstrap/00_init.sql
--
-- Creates a clean database for the case study + sanity DMVs.

IF DB_ID('dbopt_case') IS NULL
BEGIN
    PRINT 'creating database dbopt_case';
    CREATE DATABASE dbopt_case;
END
GO

USE dbopt_case;
GO

-- Enable Query Store so the analyzer can leverage it later.
IF EXISTS (SELECT 1 FROM sys.databases WHERE name = 'dbopt_case' AND is_query_store_on = 0)
BEGIN
    ALTER DATABASE dbopt_case SET QUERY_STORE = ON
        (
            OPERATION_MODE = READ_WRITE,
            INTERVAL_LENGTH_MINUTES = 5,
            DATA_FLUSH_INTERVAL_SECONDS = 60,
            QUERY_CAPTURE_MODE = AUTO,
            SIZE_BASED_CLEANUP_MODE = AUTO
        );
END
GO

-- Read-committed snapshot is recommended for the analyzer's NOLOCK alternative.
ALTER DATABASE dbopt_case SET READ_COMMITTED_SNAPSHOT ON;
GO

PRINT 'dbopt_case ready';
GO
