-- A scalar function's RETURNS type and a CLR table-valued function's RETURNS
-- TABLE signature are not stored columns; there is nothing to index or keep
-- in-row.
CREATE FUNCTION codegen.GetOpenJsonSchema (@SchemaName sysname, @TableName sysname)
RETURNS NVARCHAR(MAX)
AS
BEGIN
    RETURN N'';
END;
GO
CREATE FUNCTION tSQLt.Private_ListAnnotations (@ProcedureName NVARCHAR(MAX))
RETURNS TABLE (AnnotationNo INT, Annotation NVARCHAR(MAX))
AS EXTERNAL NAME tSQLtCLR.[tSQLtCLR.StoredProcedures].AnnotationList;
GO
