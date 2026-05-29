/**
 * Fix-log — the execution-FREE interactive remediation tracker (Pass 5 B1).
 *
 * sqlopt never runs DDL against the user's database. The remediation flow is
 * therefore a MANUAL tracker: as the user works a fix in their own SQL client
 * (SSMS / sqlcmd), they tick off the solution steps here and — once they've run
 * the script + verified it themselves — flip a "Mark validated" toggle. None of
 * this executes anything; it's a durable checklist + a user-asserted "I did
 * this" flag that survives reload and surfaces a "Validated ✓ (date)" badge back
 * on the HEALTH issue card.
 *
 * Everything is local: localStorage under `sqlopt.fixlog` (the shared `persist`
 * namespace), keyed `server·db·issue-id` — the SAME server·db scoping the
 * scanlog store uses (keyFor), so two databases on one server never cross-
 * contaminate and a fix tracked against app-sql-01 can't bleed onto another
 * instance. No PII beyond the server/db the user already typed.
 *
 * The step set is stored as an index Set serialized to a sorted number[] (Sets
 * don't JSON-serialize), so a step's "done" state is keyed by its ordinal in the
 * remediation's solution_steps list. That's stable for a given issue's templated
 * remediation; if the step list ever changes shape, stale indices simply read as
 * unticked rather than mis-ticking the wrong step.
 */

import { load, save } from "./persist";
import { keyFor } from "./scanlog";

const KEY = "fixlog";

/**
 * The persisted, per-issue tracker entry. `stepsDone` is the set of solution-step
 * ORDINALS the user has ticked. `validated` is the user's manual assertion that
 * they ran + verified the fix; `validatedAt` is the ISO timestamp it was flipped
 * on (null while unvalidated) so the card badge can read "Validated ✓ (date)".
 */
export interface FixEntry {
  /** Ordinals (0-based) of the solution_steps the user has checked off. */
  stepsDone: number[];
  /** User-asserted "I ran this fix and verified it" — NOT an executed check. */
  validated: boolean;
  /** ISO timestamp validated was last turned on, or null. */
  validatedAt: string | null;
}

/** The default (untracked) entry — no steps done, not validated. */
const EMPTY_ENTRY: FixEntry = Object.freeze({
  stepsDone: [],
  validated: false,
  validatedAt: null,
}) as FixEntry;

/** Per server·db·issue-id tracker map. */
type FixLog = Record<string, FixEntry>;

const SUBS = new Set<() => void>();

/**
 * Referentially-stable per-key cache so `useSyncExternalStore` getSnapshot hands
 * back the SAME reference between renders until a mutation bumps it — re-parsing
 * localStorage each call would return a fresh object every render and spin React
 * into an infinite loop. We parse once, then only swap a key's object on write.
 */
let CACHE: FixLog | null = null;

function hydrate(): FixLog {
  if (CACHE === null) CACHE = load<FixLog>(KEY, {});
  return CACHE;
}

function notify() {
  for (const fn of SUBS) fn();
}

/** Subscribe to mutations (tick/validate/reset) so a pane can re-render. */
export function subscribe(fn: () => void): () => void {
  SUBS.add(fn);
  return () => SUBS.delete(fn);
}

/**
 * Composite key for one tracked fix: server·db·issue-id. Reuses scanlog.keyFor
 * for the server·db prefix (unit-separator collision-safe) then appends the
 * issue id under the same separator so issue "a"+db can't collide with issue
 * "ab". Database normalizes to "" when absent (a server-wide scan).
 */
function fixKey(server: string, database: string | undefined, issueId: string): string {
  return `${keyFor(server, database)}${issueId}`;
}

/** Re-serialize the whole cache back to localStorage after a mutation. */
function persist(): void {
  if (CACHE) save(KEY, CACHE);
}

/**
 * The tracker entry for one server·db·issue (the shared frozen EMPTY_ENTRY when
 * untracked). Returns a STABLE reference until a mutation swaps it, so it's safe
 * as a `useSyncExternalStore` snapshot.
 */
export function get(server: string, database: string | undefined, issueId: string): FixEntry {
  if (!server || !issueId) return EMPTY_ENTRY;
  const log = hydrate();
  return log[fixKey(server, database, issueId)] ?? EMPTY_ENTRY;
}

/** True iff the given step ordinal is currently ticked for this issue. */
export function isStepDone(
  server: string,
  database: string | undefined,
  issueId: string,
  stepIndex: number,
): boolean {
  return get(server, database, issueId).stepsDone.includes(stepIndex);
}

/**
 * Toggle one solution step's done-state. Writes a NEW entry + new stepsDone
 * array (never mutates the cached one) so subscribers re-render. Validation is
 * left untouched — ticking steps doesn't assert the fix is validated.
 */
export function toggleStep(
  server: string,
  database: string | undefined,
  issueId: string,
  stepIndex: number,
): void {
  if (!server || !issueId) return;
  const log = hydrate();
  const k = fixKey(server, database, issueId);
  const cur = log[k] ?? EMPTY_ENTRY;
  const has = cur.stepsDone.includes(stepIndex);
  const stepsDone = has
    ? cur.stepsDone.filter((i) => i !== stepIndex)
    : [...cur.stepsDone, stepIndex].sort((a, b) => a - b);
  log[k] = { ...cur, stepsDone };
  persist();
  notify();
}

/**
 * Flip the user-asserted "Mark validated" toggle. Turning it ON stamps
 * validatedAt with the current client time (drives the "Validated ✓ (date)"
 * badge); turning it OFF clears the stamp. This is a MANUAL assertion only —
 * sqlopt does not execute anything to corroborate it.
 */
export function setValidated(
  server: string,
  database: string | undefined,
  issueId: string,
  validated: boolean,
): void {
  if (!server || !issueId) return;
  const log = hydrate();
  const k = fixKey(server, database, issueId);
  const cur = log[k] ?? EMPTY_ENTRY;
  log[k] = {
    ...cur,
    validated,
    validatedAt: validated ? new Date().toISOString() : null,
  };
  persist();
  notify();
}

/** Forget the tracker for one issue (e.g. the user wants to start over). */
export function reset(server: string, database: string | undefined, issueId: string): void {
  if (!server || !issueId) return;
  const log = hydrate();
  const k = fixKey(server, database, issueId);
  if (k in log) {
    delete log[k];
    persist();
    notify();
  }
}
