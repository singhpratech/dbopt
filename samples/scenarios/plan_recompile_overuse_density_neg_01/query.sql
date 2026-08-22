-- Three targeted hints across a long maintenance script are not "overuse".
INSERT INTO #a (x) SELECT 1;
INSERT INTO #a (x) SELECT 2;
INSERT INTO #a (x) SELECT 3;
INSERT INTO #a (x) SELECT 4;
INSERT INTO #a (x) SELECT 5;
INSERT INTO #a (x) SELECT 6;
INSERT INTO #a (x) SELECT 7;
INSERT INTO #a (x) SELECT 8;
UPDATE #a SET x = x + 1 WHERE x = 1;
UPDATE #a SET x = x + 1 WHERE x = 2;
DELETE FROM #a WHERE x = 9;
DELETE FROM #a WHERE x = 10;
SELECT x FROM #a WHERE x = @p1 OPTION (RECOMPILE);
SELECT x FROM #a WHERE x = @p2 OPTION (RECOMPILE);
SELECT x FROM #a WHERE x = @p3 OPTION (RECOMPILE);
