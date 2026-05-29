export type Severity = "info" | "warning" | "error" | "critical";

export interface Location {
  start: number;
  end: number;
  line: number;
  col: number;
}

export interface Finding {
  rule: string;
  severity: Severity;
  message: string;
  location: Location | null;
  recommendation: string | null;
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

export type IssueLane = "reliability" | "opportunity";

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

export interface HealthReport {
  engine: string;
  generated_at: string;
  window_from: string;
  window_to: string;
  connected: { server: string; database?: string };
  /** Back-compat headline = the RELIABILITY values ("are users hurting"). */
  score: number;
  grade: string;
  status: "excellent" | "good" | "fair" | "poor" | "critical" | "learning";
  /** "Are users hitting errors?" — active harm / risk to users. */
  reliability_score: number;
  reliability_grade: string;
  /** "Speed & cost to reclaim" — 100 = fully optimized, lower = more wins available. */
  efficiency_score: number;
  efficiency_grade: string;
  is_learning: boolean;
  counts: SeverityCounts;
  issues: Issue[];
  signals: SignalSummary;
}
