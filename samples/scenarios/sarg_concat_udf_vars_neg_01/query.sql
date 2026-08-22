-- Every concat operand is a UDF over variables or a literal; the compared
-- column stays bare on its own side, so nothing per-row is being built.
SELECT col.name
FROM sys.columns AS col
WHERE col.object_id = OBJECT_ID(codegen.QNAME(@SchemaName) + '.' + codegen.QNAME(@TableName));
