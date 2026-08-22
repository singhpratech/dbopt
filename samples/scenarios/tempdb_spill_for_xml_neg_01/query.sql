-- `ORDER BY ... FOR XML PATH('')` is the ordered string-aggregation idiom: the
-- ORDER BY is what makes the concatenation deterministic, and TOP/OFFSET
-- would change the answer.
SELECT @Cmd = (
    SELECT  GDIC.cmd + ';'
    FROM    DropStatements AS DS
    CROSS APPLY tSQLt.Private_GetDropItemCmd(DS.FullName, DS.ItemType) AS GDIC
    ORDER BY DS.no
    FOR XML PATH(''), TYPE
).value('.', 'nvarchar(max)');
