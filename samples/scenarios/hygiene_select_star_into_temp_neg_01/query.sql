-- A temp-table snapshot and a single-column IN subquery: all columns by intent.
SELECT * INTO #maps FROM maps;

IF EXISTS (SELECT [Action] FROM @ActionsPreferred
           WHERE [Action] NOT IN (SELECT * FROM @Actions))
    PRINT 'unsupported action';
