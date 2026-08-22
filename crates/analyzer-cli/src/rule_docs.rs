//! Static, per-rule documentation for the SARIF `tool.driver.rules[]` catalog.
//!
//! A SARIF consumer (GitHub code scanning, an editor) renders the rule
//! descriptor as "what this rule means" on EVERY alert for that rule. It must
//! therefore describe the rule, never one instance of it: the previous catalog
//! copied the first finding's message and its file-specific `CREATE INDEX`
//! DDL into the descriptor, so a finding on one table was explained with the
//! index for another, and the text changed with file order.
//!
//! Rule ids are the public contract (see `analyzer-core::rules`). An id that
//! is not in this table still gets a neutral, instance-free descriptor via
//! [`lookup`] — it never falls back to per-file text.

/// What the catalog says about one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleDoc {
    /// One sentence: what the rule detects.
    pub short: &'static str,
    /// Why it matters and what to do about it, in general terms.
    pub full: &'static str,
    /// Vendor reference page, when there is a canonical one.
    pub help_uri: Option<&'static str>,
}

const MS: &str = "https://learn.microsoft.com/sql/";

macro_rules! doc {
    ($short:expr, $full:expr) => {
        RuleDoc { short: $short, full: $full, help_uri: None }
    };
    ($short:expr, $full:expr, $uri:expr) => {
        RuleDoc { short: $short, full: $full, help_uri: Some($uri) }
    };
}

/// Look up the static descriptor for a rule id. Unknown ids get a generic
/// family-level descriptor so the catalog never carries instance text.
pub fn lookup(rule_id: &str) -> RuleDoc {
    DOCS.iter()
        .find(|(id, _)| *id == rule_id)
        .map(|(_, d)| *d)
        .unwrap_or(FALLBACK)
}

/// Build the `helpUri` for a doc, joining the MS Learn prefix.
pub fn help_uri(doc: &RuleDoc) -> Option<String> {
    doc.help_uri.map(|p| if p.starts_with("http") { p.to_string() } else { format!("{MS}{p}") })
}

const FALLBACK: RuleDoc = doc!(
    "dbopt static analysis rule",
    "A finding from the dbopt offline analyzer. The result message describes the specific \
     statement or object it was raised on; see the rule id for the family (hygiene, sarg, \
     index, joins, plan, ...)."
);

static DOCS: &[(&str, RuleDoc)] = &[
    // ---- linter-internal (emitted by the CLI itself, not analyzer-core) -----
    ("lint.unrecognized_input", doc!(
        "File could not be parsed as T-SQL or as a ShowPlanXML plan.",
        "The linter only analyses SQL text and .sqlplan XML. The file was read but its contents \
         matched neither, so no rules ran on it. Check the extension and encoding, or exclude it.")),
    ("lint.empty_file", doc!(
        "File contains no SQL after stripping whitespace and comments.",
        "An empty input is reported so a CI run that linted nothing is visible, rather than \
         silently green.")),
    // ---- hygiene -----------------------------------------------------------
    ("hygiene.select_star", doc!(
        "SELECT * returns every column instead of the ones the caller needs.",
        "Wildcard projections read and ship columns nobody uses, defeat covering indexes, and \
         silently change shape when the table changes. List the needed columns explicitly.",
        "t-sql/queries/select-transact-sql")),
    ("hygiene.nolock", doc!(
        "WITH (NOLOCK) / READ UNCOMMITTED hint allows dirty, duplicate and missing reads.",
        "NOLOCK does not make a query free: it reads uncommitted data, can return the same row \
         twice or skip rows during page splits, and can fail with error 601 under concurrent \
         modification. Prefer READ COMMITTED SNAPSHOT / SNAPSHOT isolation for non-blocking reads, \
         or fix the blocking writer instead of hiding it.",
        "t-sql/queries/hints-transact-sql-table")),
    ("hygiene.cursor", doc!(
        "Cursor iterates row-by-row where a set-based statement would do.",
        "Cursors process one row at a time with per-row overhead and long-held resources. Most \
         loops can be rewritten as a single INSERT/UPDATE/DELETE/MERGE or a window function. When a \
         cursor is truly required, declare it LOCAL FAST_FORWARD.",
        "t-sql/language-elements/declare-cursor-transact-sql")),
    ("hygiene.top_without_order_by", doc!(
        "TOP without ORDER BY returns an arbitrary, non-deterministic subset.",
        "Without an ORDER BY the engine may return any N rows and the answer can change between \
         executions or plans. Add an ORDER BY on a deterministic key, or drop TOP if the limit was \
         accidental.",
        "t-sql/queries/top-transact-sql")),
    ("hygiene.unbounded_dml", doc!(
        "UPDATE or DELETE without a WHERE clause touches every row.",
        "An unfiltered write modifies the whole table in one transaction, holds an exclusive lock \
         for its duration and can fill the log. Add a predicate, or batch the operation if the whole \
         table is really the target.",
        "t-sql/queries/update-transact-sql")),
    ("hygiene.exec_string_no_sp_executesql", doc!(
        "EXEC(@string) builds dynamic SQL without parameters.",
        "EXEC of a concatenated string produces a new plan per distinct value (plan-cache bloat) and \
         is the classic SQL-injection vector. Use sp_executesql with typed parameters.",
        "relational-databases/system-stored-procedures/sp-executesql-transact-sql")),
    ("hygiene.merge_statement_for_upsert", doc!(
        "MERGE used for a simple upsert.",
        "MERGE has a long history of correctness and concurrency issues (race conditions without \
         HOLDLOCK, trigger and filtered-index edge cases). For a plain insert-or-update, separate \
         UPDATE and INSERT ... WHERE NOT EXISTS statements inside a transaction are simpler and safer.",
        "t-sql/statements/merge-transact-sql")),
    ("hygiene.scalar_udf_in_select", doc!(
        "Scalar user-defined function called per row in a SELECT list.",
        "A scalar UDF that cannot be inlined executes once per row, serialises the plan and hides \
         its cost from the optimizer. Inline the logic, rewrite as an inline table-valued function, \
         or make the UDF eligible for scalar UDF inlining (SQL Server 2019+).",
        "relational-databases/user-defined-functions/scalar-udf-inlining")),
    ("hygiene.order_by_ordinal", doc!(
        "ORDER BY uses column positions instead of names.",
        "Ordinal ORDER BY silently changes meaning whenever the select list is edited. Name the \
         columns.",
        "t-sql/queries/select-order-by-clause-transact-sql")),
    ("hygiene.at_at_identity", doc!(
        "@@IDENTITY returns the last identity from ANY scope, including triggers.",
        "If a trigger inserts into another identity table, @@IDENTITY returns that table's value. \
         Use SCOPE_IDENTITY() or the OUTPUT clause.",
        "t-sql/functions/scope-identity-transact-sql")),
    ("hygiene.global_temp_table", doc!(
        "Global temporary table (##name) is shared across all sessions.",
        "Global temp tables are visible to every connection, collide by name and vanish when the \
         creating session ends. Use a local #temp table or a permanent staging table.",
        "t-sql/statements/create-table-transact-sql")),
    ("hygiene.heap_table", doc!(
        "CREATE TABLE without a clustered index produces a heap.",
        "Heaps accumulate forwarded records, cannot be reorganised and make every lookup a RID \
         lookup. Most OLTP tables want a narrow, static, ever-increasing clustered key.",
        "relational-databases/indexes/heaps-tables-without-clustered-indexes")),
    // ---- sargability --------------------------------------------------------
    ("sarg.function_on_column", doc!(
        "Function wrapped around a column in a predicate prevents index seeks.",
        "Applying a function to the column side of a comparison forces the engine to evaluate it \
         for every row (a scan). Move the computation to the literal/parameter side, or add a \
         persisted computed column and index it.",
        "relational-databases/sql-server-index-design-guide")),
    ("sarg.leading_wildcard", doc!(
        "LIKE with a leading wildcard ('%x') cannot seek an index.",
        "A pattern that starts with % or _ has no prefix to seek on and scans the whole index. Use \
         a trailing wildcard, a full-text index, or a reversed persisted column for suffix search.",
        "t-sql/language-elements/like-transact-sql")),
    ("sarg.implicit_convert_unicode", doc!(
        "N'...' literal compared against a non-Unicode (varchar) column.",
        "varchar has lower type precedence than nvarchar, so the column is converted on every row \
         and the index cannot be seeked. Match the literal/parameter type to the column.",
        "t-sql/data-types/data-type-precedence-transact-sql")),
    ("sarg.implicit_conversion_param_type", doc!(
        "Parameter type does not match the column it is compared to.",
        "A mismatched parameter type forces CONVERT_IMPLICIT on the column side, disables seeks and \
         distorts cardinality estimates. Declare the parameter with the column's exact type.",
        "t-sql/data-types/data-type-precedence-transact-sql")),
    ("sarg.not_in_nullable", doc!(
        "NOT IN (subquery) returns no rows if the subquery yields a NULL.",
        "Three-valued logic makes NOT IN against a nullable column silently empty. Use NOT EXISTS, \
         which is both NULL-safe and usually gets a better anti-join plan.",
        "t-sql/language-elements/exists-transact-sql")),
    ("sarg.or_chain", doc!(
        "Long OR chain across columns defeats a single index seek.",
        "ORs over different columns usually become a scan or an index-union. Consider UNION ALL of \
         per-branch queries, IN() for a single column, or a filtered index.",
        "relational-databases/sql-server-index-design-guide")),
    ("sarg.scalar_udf_in_predicate", doc!(
        "Scalar UDF in a WHERE/JOIN predicate runs per row and blocks seeks.",
        "The optimizer cannot see through a scalar UDF in a predicate: it executes per row and the \
         estimate is a constant guess. Inline the logic or use an inline TVF.",
        "relational-databases/user-defined-functions/scalar-udf-inlining")),
    ("sarg.arithmetic_on_column", doc!(
        "Arithmetic applied to a column in a predicate prevents index use.",
        "`col * 2 > @x` must be computed per row. Rewrite as `col > @x / 2` so the column stands \
         alone on one side of the comparison.",
        "relational-databases/sql-server-index-design-guide")),
    ("sarg.datetime_fn_between", doc!(
        "Date function applied to a column inside a range predicate.",
        "YEAR(col) = ..., CONVERT(date, col) BETWEEN ... and similar wrap the column and scan. \
         Express the range against the raw column: col >= @start AND col < @end.",
        "t-sql/functions/date-and-time-data-types-and-functions-transact-sql")),
    ("sarg.dateadd_on_column", doc!(
        "DATEADD applied to a column instead of to the parameter.",
        "DATEADD(day, 7, col) > @x scans; col > DATEADD(day, -7, @x) seeks. Shift the constant side.",
        "t-sql/functions/dateadd-transact-sql")),
    ("sarg.string_concat_in_predicate", doc!(
        "Columns concatenated inside a predicate cannot use an index.",
        "Comparing FirstName + ' ' + LastName to a value evaluates per row. Compare the columns \
         individually, or index a persisted computed column.",
        "relational-databases/sql-server-index-design-guide")),
    ("sarg.charindex_search_predicate", doc!(
        "CHARINDEX/PATINDEX used as a search predicate scans every row.",
        "Substring search by function has no index support. Use LIKE 'prefix%' when a prefix is \
         enough, or a full-text index for real substring search.",
        "relational-databases/search/full-text-search")),
    // ---- joins ---------------------------------------------------------------
    ("joins.comma_cross_join", doc!(
        "Comma-separated tables in FROM (old-style join).",
        "Comma joins put the join condition in WHERE, are easy to get wrong (accidental cross \
         product) and cannot express outer joins. Use explicit JOIN ... ON.",
        "t-sql/queries/from-transact-sql")),
    ("joins.join_without_on", doc!(
        "JOIN without an ON clause produces a Cartesian product.",
        "Every row of one side is paired with every row of the other. Add the join predicate, or \
         write CROSS JOIN if the product is intended.",
        "t-sql/queries/from-transact-sql")),
    ("joins.right_outer_join_readability", doc!(
        "RIGHT OUTER JOIN is harder to read than the equivalent LEFT JOIN.",
        "Swap the table order and use LEFT JOIN so the preserved table is read first; the plan \
         is identical.",
        "t-sql/queries/from-transact-sql")),
    ("joins.function_on_join_column", doc!(
        "Function applied to a column inside a JOIN condition.",
        "A wrapped join key cannot be matched by an index seek or a merge join and often degrades \
         to a hash join over full scans. Store the join key in a comparable form.",
        "relational-databases/sql-server-index-design-guide")),
    ("joins.outer_join_filtered_to_inner", doc!(
        "WHERE filter on the outer side of an OUTER JOIN turns it into an inner join.",
        "A non-NULL test on a column from the nullable side removes the rows the outer join was \
         meant to preserve. Move the condition into the ON clause, or use INNER JOIN deliberately.",
        "t-sql/queries/from-transact-sql")),
    ("joins.distinct_with_join_fanout", doc!(
        "DISTINCT used to hide row multiplication from a one-to-many join.",
        "Deduplicating after a fan-out join does extra work and may still be wrong. Filter the \
         many-side with EXISTS, or aggregate before joining.",
        "t-sql/language-elements/exists-transact-sql")),
    // ---- antipatterns ---------------------------------------------------------
    ("antipattern.union_should_be_union_all", doc!(
        "UNION performs a distinct sort that UNION ALL avoids.",
        "UNION deduplicates the combined result with a sort or hash. If the branches cannot \
         overlap, or duplicates are acceptable, use UNION ALL.",
        "t-sql/language-elements/set-operators-union-transact-sql")),
    ("antipattern.count_for_existence", doc!(
        "COUNT(*) > 0 used where EXISTS would stop at the first row.",
        "Counting touches every matching row; EXISTS short-circuits on the first one.",
        "t-sql/language-elements/exists-transact-sql")),
    ("antipattern.distinct_many_columns", doc!(
        "DISTINCT over a wide select list is usually masking a join problem.",
        "A DISTINCT across many columns forces a sort/hash over the whole result and typically \
         indicates a join that multiplies rows. Fix the join, or aggregate explicitly.",
        "t-sql/queries/select-transact-sql")),
    ("antipattern.correlated_scalar_subquery_in_select", doc!(
        "Correlated scalar subquery in the select list runs once per outer row.",
        "Each outer row re-executes the subquery. Rewrite as an OUTER APPLY, a join to a \
         pre-aggregated derived table, or a window function.",
        "t-sql/queries/from-transact-sql")),
    // ---- index (static inference) ---------------------------------------------
    ("index.missing_index_from_predicate", doc!(
        "Query filters on columns with no matching index declared in this batch.",
        "The predicate's equality and range columns do not match any index defined in the same \
         script, so the statement will scan. The result message carries an inferred CREATE INDEX \
         for that specific table; verify against the live catalog before creating it.",
        "relational-databases/indexes/tune-nonclustered-missing-index-suggestions")),
    ("index.join_filter_missing_index", doc!(
        "Join + filter column combination has no supporting index in this batch.",
        "A join key combined with a filter on the same table benefits from a composite index \
         (filter columns first, join key next). The result message carries the inferred index.",
        "relational-databases/sql-server-index-design-guide")),
    ("index.key_lookup_risk", doc!(
        "Selected columns are not covered by the index the predicate will use.",
        "A seek on a non-covering index needs a key lookup per row, which becomes a scan past a few \
         thousand rows. Add the extra columns as INCLUDE columns.",
        "relational-databases/indexes/create-indexes-with-included-columns")),
    ("index.order_by_forces_sort", doc!(
        "ORDER BY columns do not match any index order, forcing a Sort operator.",
        "Sorting a large result is memory- and tempdb-hungry. An index whose key order matches the \
         ORDER BY lets the engine stream rows in order.",
        "relational-databases/sql-server-index-design-guide")),
    ("index.guid_clustered_key", doc!(
        "Clustered index on a random GUID (uniqueidentifier) key.",
        "Random GUIDs insert all over the B-tree, causing page splits and fragmentation, and widen \
         every nonclustered index. Cluster on a narrow increasing key, or use NEWSEQUENTIALID().",
        "relational-databases/sql-server-index-design-guide")),
    ("index.wide_clustered_key", doc!(
        "Clustered key is wide (many columns or large types).",
        "Every nonclustered index carries the clustered key, so a wide key bloats them all and \
         slows lookups. Prefer a narrow surrogate key.",
        "relational-databases/sql-server-index-design-guide")),
    ("index.wide_covering_request", doc!(
        "Inferred covering index would include an unusually large set of columns.",
        "An index that INCLUDEs most of the table is nearly a second copy of it: expensive to \
         maintain and rarely worth the write cost. Narrow the select list or accept the lookup.",
        "relational-databases/indexes/create-indexes-with-included-columns")),
    ("index.missing_columnstore_opportunity", doc!(
        "Large aggregate/scan workload pattern without a columnstore index.",
        "Wide scans with GROUP BY over many rows are what columnstore is built for: 10x+ \
         compression and batch-mode execution. Consider a nonclustered columnstore index.",
        "relational-databases/indexes/columnstore-indexes-overview")),
    ("index.filtered_index_opportunity_soft_delete", doc!(
        "Queries always filter on a soft-delete/status flag; a filtered index would be smaller.",
        "When nearly every query adds IsDeleted = 0 (or a similar flag), a filtered index on the \
         live rows is far smaller and cheaper to maintain than a full one.",
        "relational-databases/indexes/create-filtered-indexes")),
    // ---- ddl -----------------------------------------------------------------
    ("ddl.fillfactor_default_zero_on_random_inserts", doc!(
        "Index on a random key with FILLFACTOR 0/100 will split pages on insert.",
        "Full pages plus random inserts mean constant page splits. Set a fill factor that leaves \
         room, or use a sequential key.",
        "relational-databases/indexes/specify-fill-factor-for-an-index")),
    ("ddl.nullable_columns_should_be_explicit", doc!(
        "Column nullability left to the session default.",
        "ANSI_NULL_DFLT settings vary per connection, so an unspecified column may be NULL in one \
         deployment and NOT NULL in another. State NULL / NOT NULL explicitly.",
        "t-sql/statements/create-table-transact-sql")),
    ("ddl.varchar_max_overuse", doc!(
        "varchar(max)/nvarchar(max) used for a column that has a bounded length.",
        "MAX types cannot be index keys, disable some online operations and push data off-row. Use \
         a sized type when the length is known.",
        "t-sql/data-types/char-and-varchar-transact-sql")),
    // ---- datatype ------------------------------------------------------------
    ("datatype.datetime_legacy", doc!(
        "Legacy datetime type; datetime2 is more precise and smaller.",
        "datetime has 3.33 ms precision and a 1753 lower bound. datetime2(n) is the recommended \
         type for new work.",
        "t-sql/data-types/datetime2-transact-sql")),
    ("datatype.float_for_money", doc!(
        "FLOAT/REAL used for a monetary or exact-decimal value.",
        "Binary floating point cannot represent most decimal fractions exactly; sums drift. Use \
         decimal(p, s).",
        "t-sql/data-types/decimal-and-numeric-transact-sql")),
    ("datatype.implicit_string_length", doc!(
        "varchar/nvarchar declared without a length.",
        "Unsized string types default to 1 (declarations) or 30 (CAST/CONVERT) characters and \
         silently truncate. Always specify a length.",
        "t-sql/data-types/char-and-varchar-transact-sql")),
    ("datatype.implicit_string_length_cast", doc!(
        "CAST/CONVERT to varchar/nvarchar without a length truncates at 30 characters.",
        "An unsized string target in CAST/CONVERT is 30 characters; longer values are cut silently. \
         Specify the length.",
        "t-sql/functions/cast-and-convert-transact-sql")),
    ("datatype.sysname_general_string", doc!(
        "sysname used for a general-purpose string column.",
        "sysname is nvarchar(128) NOT NULL intended for object names; using it for application data \
         hides the real constraint. Declare the intended type and nullability.",
        "t-sql/data-types/nchar-and-nvarchar-transact-sql")),
    // ---- deprecated ------------------------------------------------------------
    ("deprecated.outer_join_star_equal", doc!(
        "Old-style *= / =* outer join syntax is no longer supported.",
        "The *= and =* operators were removed; the statement fails under compatibility level 90+. \
         Use LEFT/RIGHT OUTER JOIN ... ON.",
        "database-engine/discontinued-database-engine-functionality-in-sql-server")),
    ("deprecated.sp_dboption", doc!(
        "sp_dboption was removed in SQL Server 2012.",
        "Use ALTER DATABASE ... SET instead.",
        "t-sql/statements/alter-database-transact-sql-set-options")),
    ("deprecated.lob_legacy_types", doc!(
        "text/ntext/image types are deprecated.",
        "These types lack most string functions, cannot be compared and will be removed. Use \
         varchar(max), nvarchar(max) or varbinary(max).",
        "t-sql/data-types/ntext-text-and-image-transact-sql")),
    ("deprecated.raiserror_legacy", doc!(
        "Legacy RAISERROR syntax (integer-first form).",
        "RAISERROR's old form is deprecated; use THROW, or the modern RAISERROR(msg, severity, \
         state) form.",
        "t-sql/language-elements/raiserror-transact-sql")),
    ("deprecated.set_rowcount_dml", doc!(
        "SET ROWCOUNT affecting INSERT/UPDATE/DELETE is deprecated.",
        "SET ROWCOUNT no longer limits DML in a future release and is ignored by some plans today. \
         Use TOP (n) on the DML statement.",
        "t-sql/statements/set-rowcount-transact-sql")),
    // ---- locking / transactions ---------------------------------------------------
    ("locking.set_transaction_isolation_read_uncommitted", doc!(
        "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED makes every read dirty.",
        "Session-wide READ UNCOMMITTED has all the hazards of NOLOCK on every statement. Use READ \
         COMMITTED SNAPSHOT or SNAPSHOT for non-blocking consistent reads.",
        "t-sql/statements/set-transaction-isolation-level-transact-sql")),
    ("locking.dml_without_batching", doc!(
        "Large single-statement DML with no batching.",
        "One giant UPDATE/DELETE escalates to a table lock, bloats the log and blocks everyone \
         until it finishes. Loop in batches of a few thousand rows with a short transaction each.",
        "relational-databases/sql-server-transaction-locking-and-row-versioning-guide")),
    ("locking.lock_escalation_disabled_globally", doc!(
        "LOCK_ESCALATION = DISABLE on a table.",
        "Disabling escalation keeps millions of row locks in memory; it is a targeted mitigation, \
         not a default. Prefer AUTO (partition-level) or fix the batching.",
        "t-sql/statements/alter-table-transact-sql")),
    ("maintenance.adr_required_for_optimized_locking", doc!(
        "Optimized locking requested without Accelerated Database Recovery.",
        "Optimized locking (SQL Server 2025+) requires ADR to be enabled on the database first.",
        "relational-databases/performance/optimized-locking")),
    ("tran.begin_without_commit", doc!(
        "BEGIN TRANSACTION with no matching COMMIT/ROLLBACK in the batch.",
        "An open transaction holds locks until the connection drops and can silently nest. Pair \
         every BEGIN with COMMIT and a ROLLBACK path.",
        "t-sql/language-elements/transactions-transact-sql")),
    ("tran.begin_without_try_catch", doc!(
        "Explicit transaction without TRY/CATCH error handling.",
        "Without TRY/CATCH (and XACT_ABORT), a runtime error can leave the transaction open. Wrap \
         the work in BEGIN TRY ... BEGIN CATCH ... ROLLBACK; THROW.",
        "t-sql/language-elements/try-catch-transact-sql")),
    ("tran.close_without_begin", doc!(
        "COMMIT/ROLLBACK with no BEGIN TRANSACTION in scope.",
        "A stray COMMIT or ROLLBACK fails at runtime (error 3902/3903) or closes a caller's \
         transaction unexpectedly. Check @@TRANCOUNT.",
        "t-sql/language-elements/transactions-transact-sql")),
    ("tran.missing_xact_abort", doc!(
        "Explicit transaction without SET XACT_ABORT ON.",
        "With XACT_ABORT OFF, many errors abort only the statement and leave the transaction open. \
         SET XACT_ABORT ON at the top of the procedure.",
        "t-sql/statements/set-xact-abort-transact-sql")),
    ("tran.ddl_inside_explicit_tran", doc!(
        "DDL executed inside an explicit user transaction.",
        "Schema changes in a user transaction take schema-modification locks for the whole \
         transaction, blocking every reader of the object. Run DDL in its own short transaction.",
        "t-sql/language-elements/transactions-transact-sql")),
    // ---- modern rewrites (version-gated) ---------------------------------------
    ("modern.missing_schema_prefix", doc!(
        "Object referenced without a schema prefix.",
        "Unqualified names are resolved per user (default schema first), which costs a lookup and \
         can bind to the wrong object. Always write schema.object.",
        "relational-databases/security/authentication-access/ownership-and-user-schema-separation")),
    ("modern.missing_set_nocount", doc!(
        "Stored procedure without SET NOCOUNT ON.",
        "Each statement sends a DONE_IN_PROC rows-affected message; in loops and chatty procedures \
         this is measurable network and client overhead. Add SET NOCOUNT ON.",
        "t-sql/statements/set-nocount-transact-sql")),
    ("modern.exec_string_concat", doc!(
        "Dynamic SQL built by string concatenation.",
        "Concatenating values into SQL text invites injection and defeats plan reuse. Parameterise \
         with sp_executesql.",
        "relational-databases/system-stored-procedures/sp-executesql-transact-sql")),
    ("modern.string_agg_replaces_for_xml", doc!(
        "FOR XML PATH('') string concatenation can be STRING_AGG (SQL Server 2017+).",
        "STRING_AGG is simpler, faster and does not need STUFF()/XML entity handling.",
        "t-sql/functions/string-agg-transact-sql")),
    ("modern.row_number_pagination_replaces_offset_fetch", doc!(
        "ROW_NUMBER() pagination can be OFFSET ... FETCH (SQL Server 2012+).",
        "OFFSET/FETCH expresses paging directly in ORDER BY and avoids the wrapping subquery.",
        "t-sql/queries/select-order-by-clause-transact-sql")),
    ("modern.greatest_least_replaces_case_when", doc!(
        "CASE WHEN a > b THEN a ELSE b END can be GREATEST/LEAST (SQL Server 2022+).",
        "GREATEST()/LEAST() are clearer and handle more than two arguments.",
        "t-sql/functions/logical-functions-greatest-transact-sql")),
    ("modern.date_bucket_replaces_floor_datediff", doc!(
        "DATEADD(DATEDIFF(...)) bucketing can be DATE_BUCKET (SQL Server 2022+).",
        "DATE_BUCKET expresses time bucketing directly and is easier to read and verify.",
        "t-sql/functions/date-bucket-transact-sql")),
    ("modern.generate_series_replaces_numbers_cte", doc!(
        "Recursive numbers CTE can be GENERATE_SERIES (SQL Server 2022+).",
        "GENERATE_SERIES is set-based and has no recursion limit or MAXRECURSION hint.",
        "t-sql/functions/generate-series-transact-sql")),
    ("modern.json_native_type_opportunity", doc!(
        "JSON stored as nvarchar(max) could use the native json type (SQL Server 2025+).",
        "The native json type validates on write, stores a compact binary form and is cheaper to \
         query than text.",
        "t-sql/data-types/json-data-type")),
    ("modern.sp_executesql_optimized_2025", doc!(
        "Dynamic SQL that can benefit from SQL Server 2025 optimized sp_executesql.",
        "SQL Server 2025 caches and reuses sp_executesql plans more aggressively; ensure the \
         statement text and parameter declarations are stable between calls.",
        "relational-databases/system-stored-procedures/sp-executesql-transact-sql")),
    // ---- plan (static hints) ------------------------------------------------------
    ("plan.scalar_udf_block_inlining", doc!(
        "Scalar UDF uses a construct that blocks scalar UDF inlining.",
        "Constructs such as GETDATE(), WITH SCHEMABINDING absence, table variables or loops make \
         the UDF ineligible for inlining in SQL Server 2019+, so it runs row-by-row.",
        "relational-databases/user-defined-functions/scalar-udf-inlining")),
    ("plan.scalar_udf_in_computed_column", doc!(
        "Scalar UDF used in a computed column definition.",
        "A UDF-backed computed column is evaluated per row on every read and forces serial plans \
         for queries touching the table. Persist the value or compute it in the application.",
        "relational-databases/user-defined-functions/scalar-udf-inlining")),
    ("plan.table_variable_large", doc!(
        "Table variable used for what is likely a large row set.",
        "Table variables have no statistics (fixed 1- or 100-row estimate before deferred \
         compilation) and cannot be indexed beyond constraints. Use a #temp table for large sets.",
        "t-sql/data-types/table-transact-sql")),
    ("plan.option_recompile_overuse", doc!(
        "OPTION (RECOMPILE) on a statement that is executed frequently.",
        "Recompiling every execution spends CPU on optimisation and prevents plan reuse. Reserve it \
         for genuinely parameter-sensitive statements.",
        "t-sql/queries/hints-transact-sql-query")),
    ("plan.optimize_for_unknown", doc!(
        "OPTIMIZE FOR UNKNOWN hint disables parameter sniffing for this statement.",
        "The average-density plan is rarely optimal for either the common or the rare value. On \
         SQL Server 2022+, Parameter Sensitive Plan optimisation handles this without hints.",
        "relational-databases/performance/parameter-sensitive-plan-optimization")),
    ("plan.merge_join_hint_pinned", doc!(
        "Explicit MERGE/HASH/LOOP join hint pins the physical join.",
        "A pinned join type stops the optimizer adapting as data volumes change and forces join \
         order. Remove the hint, or fix the statistics/index that made it seem necessary.",
        "t-sql/queries/hints-transact-sql-join")),
    ("plan.read_committed_lock_hint_redundant_with_optimized_locking", doc!(
        "READCOMMITTEDLOCK hint is redundant under optimized locking (SQL Server 2025+).",
        "With optimized locking and RCSI the hint adds lock overhead without changing semantics.",
        "relational-databases/performance/optimized-locking")),
    ("plan.recompile_defeats_psp", doc!(
        "OPTION (RECOMPILE) prevents Parameter Sensitive Plan optimisation (SQL Server 2022+).",
        "PSP keeps multiple plans per parameter bucket automatically; forcing recompiles discards \
         that benefit and pays optimisation cost every time.",
        "relational-databases/performance/parameter-sensitive-plan-optimization")),
    // ---- plan (from ShowPlanXML) ------------------------------------------------------
    ("plan.table_scan", doc!(
        "Execution plan contains a full table or clustered index scan.",
        "The operator reads every row of the object. If the query has a selective predicate, an \
         index on the filtered columns turns the scan into a seek.",
        "relational-databases/showplan-logical-and-physical-operators-reference")),
    ("plan.scan_residual_predicate", doc!(
        "Scan with a residual predicate filters rows after reading them all.",
        "The predicate is evaluated on every row rather than used to seek. An index keyed on the \
         predicate columns lets the engine read only the matching range.",
        "relational-databases/showplan-logical-and-physical-operators-reference")),
    ("plan.lookup", doc!(
        "Key or RID lookup fetches columns the index did not cover.",
        "Each qualifying row costs a second random read. Add the missing output columns as INCLUDE \
         columns on the seeking index.",
        "relational-databases/indexes/create-indexes-with-included-columns")),
    ("plan.implicit_conversion", doc!(
        "Plan shows CONVERT_IMPLICIT on a column (PlanAffectingConvert).",
        "The engine converts the column on every row, which blocks seeks and skews estimates. Match \
         the parameter/literal type to the column.",
        "t-sql/data-types/data-type-precedence-transact-sql")),
    ("plan.missing_join_predicate", doc!(
        "Plan warns NoJoinPredicate: a join is running as a Cartesian product.",
        "Some join in the statement has no condition. Add the ON clause.",
        "t-sql/queries/from-transact-sql")),
    ("plan.spill", doc!(
        "Sort or hash operator spilled to tempdb.",
        "The memory grant was too small for the actual rows, so the operator wrote to disk. Fix \
         the cardinality estimate (statistics, sargability) or reduce the rows sorted/hashed.",
        "relational-databases/showplan-logical-and-physical-operators-reference")),
    ("plan.missing_statistics", doc!(
        "Plan warns ColumnsWithNoStatistics.",
        "The optimizer guessed a cardinality for a column without statistics. Enable \
         AUTO_CREATE_STATISTICS or create the statistics explicitly.",
        "relational-databases/statistics/statistics")),
    ("plan.estimate_actual_skew", doc!(
        "Estimated rows differ from actual rows by a large factor.",
        "A bad estimate drives bad join/memory/parallelism choices. Update statistics, make the \
         predicate sargable, or check for parameter sniffing.",
        "relational-databases/performance/cardinality-estimation-sql-server")),
    ("plan.missing_index", doc!(
        "The optimizer recorded a MissingIndex suggestion in the plan.",
        "The engine itself reports which equality/inequality/include columns would have helped. \
         Review the specific index in the result message against existing indexes before creating it.",
        "relational-databases/indexes/tune-nonclustered-missing-index-suggestions")),
    ("plan.warning", doc!(
        "Execution plan carries an optimizer warning.",
        "Showplan warnings (unmatched indexes, memory grant, wait stats, ...) flag conditions the \
         optimizer could not handle well. The result message names the warning.",
        "relational-databases/showplan-logical-and-physical-operators-reference")),
    // ---- dmv / structure (bundle input) ------------------------------------------------
    ("dmv.unused_index", doc!(
        "Index with writes but no reads since the last restart.",
        "An index that is maintained on every write and never read is pure cost. Confirm over a \
         full business cycle, then drop it.",
        "relational-databases/system-dynamic-management-views/sys-dm-db-index-usage-stats-transact-sql")),
    ("dmv.duplicate_or_overlapping_index", doc!(
        "Two indexes share the same leading key columns.",
        "Overlapping indexes double write cost for no read benefit. Merge them into one index with \
         the union of INCLUDE columns.",
        "relational-databases/sql-server-index-design-guide")),
    ("dmv.missing_index", doc!(
        "The engine's missing-index DMV recorded a high-impact suggestion.",
        "sys.dm_db_missing_index_* accumulated seeks/scans that a new index would have served. The \
         result message carries the suggested index for that table.",
        "relational-databases/indexes/tune-nonclustered-missing-index-suggestions")),
    ("structure.heap_table", doc!(
        "User table has no clustered index (heap).",
        "Heaps forward rows on update and cannot be reorganised. Add a clustered index on a \
         narrow increasing key.",
        "relational-databases/indexes/heaps-tables-without-clustered-indexes")),
    ("structure.no_primary_key", doc!(
        "User table has no primary key.",
        "Without a key, rows cannot be identified for replication, change tracking or safe \
         updates. Add a primary key.",
        "relational-databases/tables/create-primary-keys")),
    ("structure.wide_clustered_key", doc!(
        "Clustered key is wide, inflating every nonclustered index.",
        "All nonclustered indexes carry the clustered key. Prefer a narrow surrogate key.",
        "relational-databases/sql-server-index-design-guide")),
    // ---- stats ---------------------------------------------------------------
    ("stats.auto_create_stats_off", doc!(
        "AUTO_CREATE_STATISTICS is being turned off.",
        "Without auto-created statistics the optimizer guesses cardinality for unindexed columns.",
        "relational-databases/statistics/statistics")),
    ("stats.auto_update_stats_off", doc!(
        "AUTO_UPDATE_STATISTICS is being turned off.",
        "Stale statistics are the most common cause of bad plans. Leave auto-update on (ASYNC if \
         the update latency hurts).",
        "relational-databases/statistics/statistics")),
    ("stats.update_statistics_fullscan_on_huge_table", doc!(
        "UPDATE STATISTICS ... WITH FULLSCAN on a very large table.",
        "A full scan of a huge table for statistics is expensive and rarely needed. Use a sample \
         rate, or PERSIST_SAMPLE_PERCENT.",
        "t-sql/statements/update-statistics-transact-sql")),
    ("stats.ascending_key_hotspot", doc!(
        "Ever-increasing key queried past the last statistics update (ascending-key problem).",
        "Rows newer than the histogram are estimated at ~1 row. Update statistics more often or rely \
         on the modern cardinality estimator's ascending-key handling.",
        "relational-databases/performance/cardinality-estimation-sql-server")),
    // ---- config --------------------------------------------------------------
    ("config.auto_close_on", doc!(
        "AUTO_CLOSE ON closes the database after the last user disconnects.",
        "Every new connection pays the full open cost and the plan cache is flushed. Set AUTO_CLOSE OFF.",
        "t-sql/statements/alter-database-transact-sql-set-options")),
    ("config.auto_shrink_on", doc!(
        "AUTO_SHRINK ON repeatedly shrinks and regrows files.",
        "Auto-shrink fragments indexes, costs I/O and the space is usually reused anyway. Set \
         AUTO_SHRINK OFF.",
        "t-sql/statements/alter-database-transact-sql-set-options")),
    ("config.page_verify_not_checksum", doc!(
        "PAGE_VERIFY is not CHECKSUM.",
        "CHECKSUM detects torn and corrupted pages on read; NONE and TORN_PAGE_DETECTION miss most \
         corruption. Set PAGE_VERIFY CHECKSUM.",
        "t-sql/statements/alter-database-transact-sql-set-options")),
    ("config.recovery_model_simple", doc!(
        "RECOVERY SIMPLE disables point-in-time restore.",
        "Under SIMPLE recovery the log is truncated on checkpoint, so only full/differential restores \
         are possible. Confirm the RPO really allows it.",
        "relational-databases/backup-restore/recovery-models-sql-server")),
    ("config.dbcc_shrink", doc!(
        "DBCC SHRINKDATABASE / SHRINKFILE in a script.",
        "Shrinking fragments every index it moves and the file usually regrows. Reserve it for \
         one-off reclaim after a large delete, followed by index maintenance.",
        "t-sql/database-console-commands/dbcc-shrinkdatabase-transact-sql")),
    ("config.dbcc_traceon_global", doc!(
        "DBCC TRACEON(..., -1) enables a trace flag server-wide from a script.",
        "Global trace flags set ad hoc are not persisted and are easy to forget. Use startup \
         parameters or database-scoped configuration.",
        "t-sql/database-console-commands/dbcc-traceon-transact-sql")),
    ("config.maxdop_zero", doc!(
        "max degree of parallelism set to 0 (unlimited).",
        "MAXDOP 0 lets one query take every scheduler. Follow the documented guideline (typically \
         8, or cores per NUMA node).",
        "database-engine/configure-windows/configure-the-max-degree-of-parallelism-server-configuration-option")),
    ("config.cost_threshold_default", doc!(
        "cost threshold for parallelism left at the default of 5.",
        "A threshold of 5 parallelises trivially cheap queries. Most OLTP systems start at 25-50 and \
         tune from there.",
        "database-engine/configure-windows/configure-the-cost-threshold-for-parallelism-server-configuration-option")),
    // ---- security --------------------------------------------------------------
    ("security.xp_cmdshell", doc!(
        "xp_cmdshell enabled or invoked.",
        "xp_cmdshell runs OS commands as the service account. Keep it disabled; use SQL Agent proxies \
         or external jobs with least privilege.",
        "relational-databases/system-stored-procedures/xp-cmdshell-transact-sql")),
    ("security.grant_to_public", doc!(
        "Permission granted to the public role.",
        "Everything granted to public is granted to every login. Grant to a specific role.",
        "t-sql/statements/grant-transact-sql")),
    ("security.grant_control", doc!(
        "CONTROL permission granted on an object, schema or database.",
        "CONTROL is ownership-equivalent. Grant the specific permissions needed.",
        "t-sql/statements/grant-transact-sql")),
    ("security.grant_with_grant_option", doc!(
        "Permission granted WITH GRANT OPTION.",
        "The grantee can re-grant the permission to anyone, so the permission chain is no longer \
         auditable. Drop the option.",
        "t-sql/statements/grant-transact-sql")),
    ("security.add_to_privileged_role", doc!(
        "Principal added to sysadmin / securityadmin / db_owner.",
        "Fixed high-privilege roles bypass every permission check. Prefer granular grants.",
        "relational-databases/security/authentication-access/server-level-roles")),
    ("security.execute_as_without_revert", doc!(
        "EXECUTE AS without a matching REVERT.",
        "The impersonation context stays in effect for the rest of the session or module.",
        "t-sql/statements/execute-as-transact-sql")),
    ("security.openrowset_inline_credentials", doc!(
        "OPENROWSET/OPENDATASOURCE with an inline user name and password.",
        "Credentials in SQL text end up in the plan cache, Query Store and source control. Use a \
         database-scoped credential or a linked server.",
        "t-sql/functions/openrowset-transact-sql")),
    // ---- tempdb --------------------------------------------------------------
    ("tempdb.spill_risk_large_sort", doc!(
        "Sort over a large, unindexed set is likely to spill to tempdb.",
        "An ORDER BY / DISTINCT / GROUP BY with no supporting index sorts the whole set in memory \
         and spills when the grant is exceeded. Index the sort columns or reduce the rows.",
        "relational-databases/showplan-logical-and-physical-operators-reference")),
    ("tempdb.large_in_clause_constant_list", doc!(
        "IN (...) with a very long constant list.",
        "Hundreds of literals produce a huge, uncached plan and can hit nesting limits. Pass the \
         values as a table-valued parameter or a temp table.",
        "t-sql/language-elements/in-transact-sql")),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_rule_has_static_text_and_uri() {
        let d = lookup("hygiene.nolock");
        assert!(d.short.contains("NOLOCK"));
        assert_eq!(
            help_uri(&d).as_deref(),
            Some("https://learn.microsoft.com/sql/t-sql/queries/hints-transact-sql-table")
        );
    }

    #[test]
    fn unknown_rule_gets_neutral_fallback_never_instance_text() {
        let d = lookup("made.up_rule");
        assert_eq!(d, FALLBACK);
        assert!(help_uri(&d).is_none());
        assert!(!d.full.contains("CREATE INDEX"));
    }

    #[test]
    fn no_descriptor_embeds_per_file_ddl() {
        // The whole point of a static catalog: no descriptor can name a
        // specific table or carry a CREATE INDEX statement.
        for (id, d) in DOCS {
            for text in [d.short, d.full] {
                assert!(!text.contains("CREATE NONCLUSTERED INDEX"), "{id} carries DDL");
                assert!(!text.contains("dbo."), "{id} names a concrete object");
            }
        }
    }

    #[test]
    fn table_ids_are_unique() {
        let mut ids: Vec<&str> = DOCS.iter().map(|(id, _)| *id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate rule id in DOCS");
    }
}
