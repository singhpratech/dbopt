-- Bracketed XML method on a nodes() alias: [c].[value]() is not a UDF.
SELECT [mps].[name],
       [c].[value]('(@dts:ObjectName)', 'NVARCHAR(128)') AS [step_name]
FROM [maintenance_plan_steps] AS [mps]
CROSS APPLY [mps].[plan_xml].[nodes]('//dts:Executables/dts:Executable') AS [t]([c]);
