# plan_missing_index_pos_01

A **real** estimated plan captured from SQL Server 2025 (`SET SHOWPLAN_XML ON`)
against a 4.1M-row heap, then anonymized. Object and column names were renamed;
the XML shape is untouched.

Why this scenario exists: every hand-written plan fixture in this repo wrote
`<Column … />` in the self-closing form, so the parser only ever handled
`Event::Empty`. Real SQL Server emits the **paired** form:

    <Column Name="[Kind]" ColumnId="4"></Column>

Every missing-index column was therefore silently dropped in the field, and the
tool shipped `CREATE NONCLUSTERED INDEX … ( /* no key columns reported */ )` —
uncompilable DDL sitting on the product's strongest feature. It was found in a
production trial, not by the test suite, because the test suite had never seen a
plan SQL Server actually wrote.

Keep this fixture byte-shaped as captured. If you regenerate it, do not
"tidy" the self-closing tags.
