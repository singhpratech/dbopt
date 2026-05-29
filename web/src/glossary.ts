/**
 * Plain-English glossary for the whole app. Every bit of SQL-Server jargon the
 * UI surfaces (DMVs, SARGability, grades, wait types, …) should be explainable
 * here so we never leave the user staring at an unexplained acronym.
 *
 * Entries are keyed by a lowercase slug. <Term k="sargable">…</Term> and the
 * HelpPanel both read from GLOSSARY[k]; an unknown key renders its children
 * plain, so adding new terms is non-breaking.
 */
export interface GlossaryEntry {
  /** Human-readable display name, e.g. "Dynamic Management View". */
  term: string;
  /** One- to two-sentence plain-English definition (shown in tooltips). */
  short: string;
  /** Optional longer explanation (shown in the Help panel glossary). */
  long?: string;
  /** Optional "Learn more" link to canonical docs. */
  docUrl?: string;
}

export const GLOSSARY: Record<string, GlossaryEntry> = {
  dmv: {
    term: "Dynamic Management View (DMV)",
    short:
      "Built-in SQL Server views exposing live performance stats — what's running, waits, index usage. We read these to diagnose your database.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/system-dynamic-management-views",
  },
  sargable: {
    term: "SARGable",
    short:
      "Search-ARGument-able — a WHERE clause SQL Server can satisfy with an index seek.",
    long:
      "Search-ARGument-able — a WHERE clause SQL Server can satisfy with an index seek. Wrapping a column in a function (e.g. UPPER(col)=) makes it non-SARGable, forcing a full scan.",
  },
  columnstore: {
    term: "Columnstore index",
    short:
      "A column-oriented index. For large scan/analytic tables it compresses ~5–10× and scans far faster than the default row-by-row (rowstore) format.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/relational-databases/indexes/columnstore-indexes-overview",
  },
  cardinality: {
    term: "Cardinality estimate",
    short:
      "The optimizer's estimate of how many rows a step returns. Bad estimates → slow plans.",
  },
  wait_type: {
    term: "Wait type",
    short:
      "What SQL Server was waiting on instead of doing work (disk, locks, CPU, memory). The top wait points at the bottleneck.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-os-wait-stats-transact-sql",
  },
  regression: {
    term: "Query regression",
    short: "A query that has gotten slower than its own recent baseline.",
  },
  blocking: {
    term: "Blocking",
    short:
      "One session holds a lock that forces others to wait — their queries hang or time out.",
  },
  deadlock: {
    term: "Deadlock",
    short:
      "Two+ transactions each wait on a lock the other holds; SQL Server kills one (the 'victim') so the rest proceed — the victim's transaction fails and rolls back.",
  },
  missing_index: {
    term: "Missing index",
    short:
      "A query repeatedly scans a whole table because no index supports its filter; adding one turns the scan into a fast seek.",
  },
  unused_index: {
    term: "Unused index",
    short:
      "An index that is written on every change but never read — pure write/storage cost. Candidate to drop.",
  },
  duplicate_index: {
    term: "Duplicate index",
    short:
      "An index that overlaps another; the redundant one doubles write cost for no read benefit.",
  },
  reliability_grade: {
    term: "Reliability grade",
    short:
      "Are users hitting errors right now? Driven by deadlocks, blocking, harmful waits, and query regressions.",
  },
  efficiency_grade: {
    term: "Efficiency grade",
    short:
      "How much speed and storage you could reclaim — index and columnstore opportunities. Lower = more easy wins available, not 'broken'.",
  },
  learning_mode: {
    term: "Learning mode",
    short:
      "We haven't observed this server long enough yet (stats reset on restart), so we don't penalize the grade — absence of signal isn't proof of health.",
  },
  impact_rank: {
    term: "Impact rank",
    short:
      "Our 0–10,000 ranking of how much an issue matters, from the underlying metric (e.g. estimated rows scanned, deadlock count). Higher = fix first.",
  },
  query_store: {
    term: "Query Store",
    short:
      "SQL Server's built-in flight recorder — it persists query plans and runtime stats over time, so we can spot regressions and compare a query against its own baseline.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/relational-databases/performance/monitoring-performance-by-using-the-query-store",
  },
  rcsi: {
    term: "Read Committed Snapshot Isolation (RCSI)",
    short:
      "A database setting where readers see a consistent snapshot instead of taking shared locks — so reads no longer block writes (and vice-versa), cutting most blocking.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/t-sql/statements/set-transaction-isolation-level-transact-sql",
  },
  maxdop: {
    term: "MAXDOP (max degree of parallelism)",
    short:
      "The cap on how many CPU cores one query may use in parallel. Set too high it causes CXPACKET waits and noisy neighbours; too low it serialises big queries.",
    docUrl:
      "https://learn.microsoft.com/en-us/sql/database-engine/configure-windows/configure-the-max-degree-of-parallelism-server-configuration-option",
  },
  heap: {
    term: "Heap",
    short:
      "A table with no clustered index — rows are stored in no particular order, so most lookups become full scans. Usually wants a clustered index.",
  },
  clustered_index: {
    term: "Clustered index",
    short:
      "The index that defines the physical row order of a table (only one per table). Its key is how rows are actually stored on disk.",
  },
  scan_vs_seek: {
    term: "Scan vs seek",
    short:
      "A seek jumps straight to the matching rows via an index (fast); a scan reads the whole table/index (slow on big tables). Turning scans into seeks is the core index win.",
  },
};
