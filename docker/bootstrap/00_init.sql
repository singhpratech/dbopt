-- Run with: sqlcmd -S localhost,14333 -U sa -P "$SA_PASSWORD" -C -i docker/bootstrap/00_init.sql
--
-- Creates a clean database for the case study + sanity DMVs.

IF DB_ID('sqlopt_case') IS NULL
BEGIN
    PRINT 'creating database sqlopt_case';
    CREATE DATABASE sqlopt_case;
END
GO

USE sqlopt_case;
GO

-- Enable Query Store so the analyzer can leverage it later.
IF EXISTS (SELECT 1 FROM sys.databases WHERE name = 'sqlopt_case' AND is_query_store_on = 0)
BEGIN
    ALTER DATABASE sqlopt_case SET QUERY_STORE = ON
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
ALTER DATABASE sqlopt_case SET READ_COMMITTED_SNAPSHOT ON;
GO

PRINT 'sqlopt_case ready';
GO
