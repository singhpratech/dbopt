export type Severity = "info" | "warning" | "error" | "critical";

export interface Location {
  start: number;
  end: number;
  line: number;
  col: number;
}

/** The database object a finding is about. Absent when the rule matched text
 *  rather than an object (most token rules) — never inferred from the message.
 *  `row_count` / `reserved_kb` are present only when measured. */
export interface ObjectRef {
  schema: string;
  table: string;
  index?: string;
  row_count?: number;
  reserved_kb?: number;
}

export interface Finding {
  rule: string;
  severity: Severity;
  message: string;
  location: Location | null;
  recommendation: string | null;
  /** Optional: omitted by the backend when the rule has no object identity. */
  object?: ObjectRef;
}

export interface TreemapNode {
  name: string;
  value: number;
  physical_op: string;
  logical_op: string;
  estimated_rows: number;
  children: TreemapNode[];
}

export interface HeatmapCell {
  row: string;
  col: string;
  seeks: number;
  scans: number;
  lookups: number;
  updates: number;
  score: number;
}

export interface SizeNode {
  schema: string;
  table: string;
  index: string;
  row_count: number;
  reserved_kb: number;
  used_kb: number;
  data_kb: number;
}

export interface SeverityBucket {
  line: number;
  critical: number;
  error: number;
  warning: number;
  info: number;
}

export interface ChartData {
  plan_treemap: TreemapNode[];
  index_heatmap: HeatmapCell[];
  size_treemap: SizeNode[];
  severity_timeline: SeverityBucket[];
}

export interface AnalysisReport {
  findings: Finding[];
  charts: ChartData;
}

export interface ConnectionInfo {
  server: string;
  database?: string;
  user?: string;
  password?: string;
  trust_cert?: boolean;
  /** "sql" | "integrated" | "windows". Absent ⇒ backend infers from `user`. */
  auth_mode?: string;
}

/* ============================================================
   Health front-door — engine-neutral aggregated report.
   Snake_case wire contract (matches ConnectResp/WeeklyReport serde).
   The backend fuses advisor recs + static findings + sentinel pain
   into a flat, pre-ranked Issue[]; the frontend renders this only,
   never RecKind/DMV internals.
   ============================================================ */
export type IssueSeverity = "critical" | "error" | "warning" | "info";

export type FixAction = "execute" | "review" | "investigate";

export type IssueLane = "reliability" | "opportunity" | "operational";

/**
 * Provenance of an Issue's numbers, so we never imply fake precision:
 *  • observed   — measured directly from DMV counters (writes, deadlock count).
 *  • estimated  — SQL Server's OWN projection (e.g. missing-index avg impact).
 *  • heuristic  — a rule-of-thumb (columnstore compression ratios). Verify first.
 */
export type Confidence = "observed" | "estimated" | "heuristic";

/**
 * One evidence chip — a grounded label/value pair lifted from the same DMV data
 * used to build the rationale (e.g. {label:"Writes maintained", value:"412/wk"}).
 * Pre-formatted server-side (MB/GB, thousands-separated); rendered verbatim.
 */
export interface Metric {
  label: string;
  value: string;
  /**
   * Provenance of THIS metric — the DMV/origin it was measured from, e.g.
   * "sys.dm_db_partition_stats" or "system_health XEvents". Optional (serde
   * default None) so older backends/issues without it still render; the chip
   * popover surfaces it as "Measured from <source>" when present.
   */
  source?: string;
}

export interface Issue {
  id: string;
  source: "advisor" | "sentinel" | "static";
  kind: string;
  severity: IssueSeverity;
  /** Which grade this issue counts against: active harm vs. a faster/cheaper win. */
  lane: IssueLane;
  /** One plain-English sentence of user impact, shown prominently on the card. */
  consequence: string;
  impact_rank: number;
  title: string;
  affected_object: string;
  rationale: string;
  fix_sql?: string;
  fix_action: FixAction;
  /** Grounded evidence chips (may be empty). Pre-formatted server-side. */
  metrics: Metric[];
  /** Provenance band for the metrics/impact — defaults "observed". */
  confidence: Confidence;
}

export interface SeverityCounts {
  critical: number;
  error: number;
  warning: number;
  info: number;
}

export interface SignalSummary {
  missing_indexes: number;
  unused_indexes: number;
  duplicate_indexes: number;
  columnstore_candidates: number;
  top_wait_type?: string;
  top_wait_time_ms: number;
  deadlock_count: number;
  blocking_incidents: number;
  regressed_queries: number;
}

/** "Today vs rolling baseline" trend behind the health grade, from the durable
 *  per-query baseline. Absent (UI: "baseline forming") until a query has a
 *  mature baseline — every value is measured, never guessed. */
export interface BaselineTrend {
  tracked_queries: number;
  baseline_mean_ms: number;
  current_mean_ms: number;
  /** Positive = slower than this instance's own normal. */
  delta_pct: number;
  worst_z_score: number;
  /** "stale" overrides the z-band when the baseline predates the report window. */
  band: "steady" | "elevated" | "regressed" | "stale";
  /** ISO timestamp: when the sentinel last folded a sample into any tracked baseline. */
  last_updated: string;
  /** True when last_updated is older than the report window — render "baseline stale", never a trend. */
  stale: boolean;
}

export interface HealthReport {
  engine: string;
  generated_at: string;
  window_from: string;
  window_to: string;
  connected: { server: string; database?: string };
  /** Headline = the WORST of the three lanes (reliability / efficiency / operational). */
  score: number;
  grade: string;
  status: "excellent" | "good" | "fair" | "poor" | "critical" | "learning";
  /** "Are users hitting errors?" — active harm / risk to users. */
  reliability_score: number;
  reliability_grade: string;
  /** "Speed & cost to reclaim" — 100 = fully optimized, lower = more wins available. */
  efficiency_score: number;
  efficiency_grade: string;
  /** "Can you recover it?" — backups, integrity checks, config best-practices. 100 = clean or not checked. */
  operational_score: number;
  operational_grade: string;
  is_learning: boolean;
  /** Seconds of sentinel history backing this window; absent when the newest capture predates the window. */
  monitoring_age_secs?: number | null;
  /** Seconds since the NEWEST sentinel capture; absent when nothing was ever captured. */
  last_capture_secs?: number | null;
  /** "Today vs rolling baseline" trend behind the grade. Absent until a query
   *  has a mature durable baseline ("baseline forming"). */
  baseline_trend?: BaselineTrend;
  /** Seconds since the DMV usage counters were last reset (SQL restart / DB
   *  state change). Absent on older backends; null when unknown. Young
   *  counters (< 24h) make every usage-based verdict provisional. */
  counter_age_secs?: number | null;
  /** ISO timestamp of that counter reset — "counters since …". */
  counters_since?: string | null;
  counts: SeverityCounts;
  issues: Issue[];
  signals: SignalSummary;
}

/* ============================================================
   Issue Detail + Remediation — the deep view behind a clicked
   Issue card. ONE Remediation object renders uniformly whether
   it is BACKEND-ENRICHED (deadlock/blocking/wait/regression, via
   POST /api/health/issue/detail) or FRONTEND-TEMPLATED from
   fields already on the Issue (the four advisor kinds + finding).

   Shape mirrors the Rust serde structs in
   crates/backend/src/health/enrichment.rs (snake_case wire).
   ============================================================ */

/** One ordered, imperative remediation step; may carry copy-paste T-SQL. */
export interface RemediationStep {
  /** Imperative step label. */
  title: string;
  /** Optional elaboration / rationale for the step. */
  detail?: string;
  /** Optional copy-paste T-SQL for this step. */
  sql?: string;
}

/** Coarse, explainable risk band — never a fake-precise score (playbook §3). */
export type RiskLevel = "safe" | "moderate" | "risky";

/**
 * One rung of a ranked solution ladder for the investigate kinds
 * (deadlock/blocking/wait/regression). rank 0 = safest / most-likely first.
 * Each rung pairs benefit (estimated_impact) with cost/caveat (notes) so the
 * problem and the fix share a currency (playbook §2/§4).
 */
export interface SolutionOption {
  /** 0 = safest / most-likely first. */
  rank: number;
  /** "index" | "lock-order" | "isolation" | "txn-scope" | "stats" | "query-rewrite" | … */
  category: string;
  description: string;
  /** Null/omitted for procedural (non-DDL) fixes. */
  sql_fix?: string;
  risk_level: RiskLevel;
  estimated_impact: string;
  notes: string;
}

/**
 * The complete remediation contract for ONE issue. diagnosis → solution →
 * apply_safely → validate → rollback → impact, in that order (because-before-fix,
 * playbook §1). advisor kinds omit `solutions`; deadlock carries `supplemental`
 * (the parsed graph JSON) for power-user verification.
 */
export interface Remediation {
  /** FK to Issue.id. */
  issue_id: string;
  /** Mirror of Issue.kind. */
  issue_kind: string;
  /** Root-cause prose (extends Issue.rationale). */
  diagnosis: string;
  /** Ordered human steps — always present. */
  solution_steps: RemediationStep[];
  /** Ranked ladder for investigate-kinds; omitted for advisor kinds. */
  solutions?: SolutionOption[];
  /** Primary executable DDL (advisor kinds: = Issue.fix_sql). */
  fix_sql?: string;
  /** Pre-flight checklist (gates / preconditions). */
  apply_safely: string[];
  /** Post-change verification queries / checks (prove-it-worked loop). */
  validate: string[];
  /** Undo steps / inverse DDL — mandatory output, never optional. */
  rollback: string[];
  /** One-line expected effect + honest confidence. */
  impact: string;
  /** Deadlock-only: parsed graph JSON for power users (raw artifact). */
  supplemental?: unknown;
}
