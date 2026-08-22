-- A short-lived #temp staging table is the exception the rule text itself
-- names; reporting every work table in a diagnostic proc is noise.
CREATE TABLE #dbcc_events_from_trace (
    StartTime    datetime      NOT NULL,
    TextData     nvarchar(4000) NULL
);
