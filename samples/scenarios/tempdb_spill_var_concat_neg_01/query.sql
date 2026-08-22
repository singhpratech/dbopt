-- `SELECT @v = @v + ... ORDER BY` builds a parameter list in key-column order.
-- The ORDER BY is semantically required; there is no sort to bound.
SELECT @Parameters = @Parameters + '@' + c.ColumnName + ' ' + c.ColumnType + ','
FROM   codegen.GetPkColumns(@SchemaName, @TableName) AS c
ORDER BY c.ColumnId;
