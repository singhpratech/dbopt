/**
 * Scan-history / fix-audit trail (Phase B — the persistent trust loop).
 *
 * The Pass-3 re-scan delta lived only in memory and auto-dismissed after ~6s, so
 * the "did my fix actually move the needle?" proof vanished on reload and never
 * spanned sessions. This store keeps a durable trail in localStorage: the last
 * ~10 HealthReport snapshots PER server·db, each captured down to the set of
 * issue ids+titles. That lets us compute an ISSUE-LEVEL diff on every re-scan
 * (resolved/new titles, not just an aggregate grade swing) and render a compact
 * trend table — without any backend or extra round-trip.
 *
 * Everything is local: localStorage under `sqlopt.scanlog` (the shared `persist`
 * namespace), keyed `serverdatabase` so two databases on one server never
 * cross-contaminate. No PII beyond the server/db the user already typed.
 */

import { load, save } from "./persist";
import type { HealthReport } from "../api/backend";

/** How many snapshots we retain per server·db. Oldest beyond this is dropped. */
const MAX_SNAPSHOTS = 10;

const KEY = "scanlog";

/** A single issue distilled to what a diff needs (id for identity, title for prose). */
export interface SnapshotIssue {
  id: string;
  title: string;
}

/** One captured HEALTH scan, trimmed to the fields the audit trail needs. */
export interface ScanSnapshot {
  /** ISO timestamp of when this scan was captured (client clock). */
  at: string;
  reliability_score: number;
  reliability_grade: string;
  efficiency_score: number;
  efficiency_grade: string;
  /** Was the server still in learning mode at capture time? */
  is_learning: boolean;
  issues: SnapshotIssue[];
}

/** Per-server·db history: newest first. */
type ScanLog = Record<string, ScanSnapshot[]>;

const SUBS = new Set<() => void>();

/**
 * Referentially-stable snapshot cache, keyed by server·db. `useSyncExternalStore`
 * requires getSnapshot to return the SAME reference between renders until the
 * store actually changes — re-parsing localStorage each call would hand back a
 * fresh array every render and spin React into an infinite loop. We parse once,
 * cache the per-key array, and only bump the reference on append/clear.
 */
const SNAP_CACHE = new Map<string, ScanSnapshot[]>();
let HYDRATED = false;

function hydrateCache(): void {
  if (HYDRATED) return;
  const all = load<ScanLog>(KEY, {});
  for (const k of Object.keys(all)) SNAP_CACHE.set(k, all[k] ?? []);
  HYDRATED = true;
}

function notify() {
  for (const fn of SUBS) fn();
}

/** Subscribe to mutations (append/clear) so a panel can re-render the table. */
export function subscribe(fn: () => void): () => void {
  SUBS.add(fn);
  return () => SUBS.delete(fn);
}

/**
 * Stable per-database key. Uses a unit-separator so a server literally named
 * "a" + db "b" can't collide with server "ab". Database is normalized to
 * "" when absent (a server-wide scan) so it keys consistently.
 */
export function keyFor(server: string, database?: string): string {
  return `${server}${database ?? ""}`;
}

/** A shared frozen empty array so a no-history key returns a STABLE reference. */
const EMPTY = Object.freeze([] as ScanSnapshot[]) as ScanSnapshot[];

/** Re-serialize the whole cache back to localStorage after a mutation. */
function persist(): void {
  const all: ScanLog = {};
  for (const [k, v] of SNAP_CACHE) all[k] = v;
  save(KEY, all);
}

/**
 * The retained snapshots for a server·db, newest first (empty if none).
 * Returns the cached reference so repeated calls (e.g. from useSyncExternalStore)
 * are stable until an append/clear bumps it.
 */
export function history(server: string, database?: string): ScanSnapshot[] {
  if (!server) return EMPTY;
  hydrateCache();
  return SNAP_CACHE.get(keyFor(server, database)) ?? EMPTY;
}

/** The most recent prior snapshot for a server·db, or null on first ever scan. */
export function latest(server: string, database?: string): ScanSnapshot | null {
  return history(server, database)[0] ?? null;
}

/** Distill a live HealthReport into the trimmed snapshot we persist. */
function toSnapshot(report: HealthReport): ScanSnapshot {
  return {
    at: new Date().toISOString(),
    reliability_score: report.reliability_score,
    reliability_grade: report.reliability_grade ?? "?",
    efficiency_score: report.efficiency_score,
    efficiency_grade: report.efficiency_grade ?? "?",
    is_learning: report.is_learning === true,
    issues: (report.issues ?? []).map((i) => ({ id: i.id, title: i.title })),
  };
}

/**
 * Append a successful scan to the trail and return the snapshot we stored
 * (so the caller can keep it without re-reading). Trims to MAX_SNAPSHOTS.
 */
export function append(server: string, database: string | undefined, report: HealthReport): ScanSnapshot {
  const snap = toSnapshot(report);
  if (!server) return snap; // nothing to key against — don't persist garbage.
  hydrateCache();
  const k = keyFor(server, database);
  // New array reference (newest first, trimmed) so subscribers re-render.
  const list = [snap, ...(SNAP_CACHE.get(k) ?? [])].slice(0, MAX_SNAPSHOTS);
  SNAP_CACHE.set(k, list);
  persist();
  notify();
  return snap;
}

/** Forget the entire trail for one server·db (the "Clear history" control). */
export function clear(server: string, database?: string): void {
  hydrateCache();
  const k = keyFor(server, database);
  if (SNAP_CACHE.has(k)) {
    SNAP_CACHE.delete(k);
    persist();
    notify();
  }
}

/** The grade move for one axis between two scans. */
export interface AxisDiff {
  fromGrade: string;
  toGrade: string;
  fromScore: number;
  toScore: number;
}

/**
 * The ISSUE-LEVEL diff between a prior snapshot and the current report.
 *  • resolved — issues present before that are gone now (the fixes that landed).
 *  • added    — issues new since last scan (regressions / freshly surfaced).
 * Titles come along so the strip can name them ("Resolved: 2 — Missing index…").
 */
export interface ScanDiff {
  resolved: SnapshotIssue[];
  added: SnapshotIssue[];
  reliability: AxisDiff;
  efficiency: AxisDiff;
  /** ISO timestamp of the prior scan we diffed against (for "since <time>"). */
  prevAt: string;
}

/**
 * Compute the issue-level + grade diff between a prior snapshot and the live
 * report. Identity is by issue id (stable post-dedup); titles are taken from
 * whichever side still has the issue.
 */
export function diff(prev: ScanSnapshot, report: HealthReport): ScanDiff {
  const now = toSnapshot(report);
  const prevIds = new Map(prev.issues.map((i) => [i.id, i] as const));
  const nowIds = new Map(now.issues.map((i) => [i.id, i] as const));

  const resolved: SnapshotIssue[] = [];
  for (const i of prev.issues) if (!nowIds.has(i.id)) resolved.push(i);

  const added: SnapshotIssue[] = [];
  for (const i of now.issues) if (!prevIds.has(i.id)) added.push(i);

  return {
    resolved,
    added,
    reliability: {
      fromGrade: prev.reliability_grade,
      toGrade: now.reliability_grade,
      fromScore: prev.reliability_score,
      toScore: now.reliability_score,
    },
    efficiency: {
      fromGrade: prev.efficiency_grade,
      toGrade: now.efficiency_grade,
      fromScore: prev.efficiency_score,
      toScore: now.efficiency_score,
    },
    prevAt: prev.at,
  };
}
