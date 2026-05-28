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
