-- `.exist()`, `.nodes()` and `.value()` are methods of the xml type. They
-- share the `alias.name(` shape of a scalar UDF and are nothing of the kind.
SELECT c.n.value('@EstimateCPU', 'float') AS estimated_cpu,
       dp.process_xml.value('(//process/inputbuf/text())[1]', 'nvarchar(max)') AS input_buffer
FROM #deadlock AS dp
CROSS APPLY dp.process_xml.nodes('//process') AS c(n)
WHERE dp.process_xml.exist('@timestamp[. >= sql:variable("@StartDate")]') = 1
  AND c.n.exist('/p:RelOp/p:ComputeScalar/p:DefinedValues') = 0;
