-- The predicate targets the output of sys.fn_trace_gettable(), a table-valued
-- function over the default trace file. A TVF result has no statistics.
INSERT INTO #trace_events (EventClass, StartTime, LoginName)
SELECT  t.EventClass, t.StartTime, t.LoginName
FROM    sys.fn_trace_gettable(@base_tracefilename, DEFAULT) AS t
WHERE
        (
            t.EventClass = 22
            AND t.Severity >= 17
            AND t.StartTime > DATEADD(dd, -30, GETDATE())
        )
        OR
        (
            t.EventClass IN (92, 93)
            AND t.StartTime > DATEADD(dd, -30, GETDATE())
        );
