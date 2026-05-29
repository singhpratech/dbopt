//! Deadlock enrichment — the centerpiece.
//!
//! We persist the WHOLE `system_health` ring-buffer target as one blob
//! (`poll/deadlocks.rs`) containing many `<event>` entries, each wrapping a
//! `<deadlock-graph>`. This module parses that blob with quick-xml's serde
//! deserializer, reconstructs the cycle (victim SPID, participating processes +
//! their inputbuf SQL, lock modes, resource owner→waiter edges), and synthesizes
//! a ranked, evidence-backed fix ladder — instead of the old "N deadlocks" count.
//!
//! HONESTY (playbook #8 / deadlockApproach): Extended Events can mis-attribute or
//! truncate the captured statement, and parallel-deadlock detail can be
//! incomplete. We say so in the caveats and always ship the parsed graph as
//! `supplemental` so a power user can verify our reading against the raw facts.
//!
//! GRACEFUL DEGRADATION is non-negotiable (SQL 2014→2025 XML varies): no row, an
//! unreadable store, or a parse error all fall back to the generic ladder and a
//! warn-log — never a 500.

use serde::Deserialize;

use super::db::ReadStore;
use super::{relative_age, Remediation, RemediationStep, SolutionOption};
use crate::health::enrichment::IssueDetailReq;
use sentinel::storage::TimeRange;

// ===========================================================================
// quick-xml serde structs for the system_health ring buffer → deadlock-graph.
//
// The ring buffer serializes as <RingBufferTarget><event>…</event>…>. Each
// event carries <data name="xml_report"><value><deadlock-graph>…. Depending on
// SQL Server version the <value> body is sometimes real child XML and sometimes
// XML-as-escaped-text, so we deserialize tolerantly and, if no graph was found
// as children, re-parse the inner text (see `extract_last_graph`).
// ===========================================================================

#[derive(Debug, Deserialize)]
struct RingBufferTarget {
    #[serde(rename = "event", default)]
    events: Vec<RbEvent>,
}

#[derive(Debug, Deserialize)]
struct RbEvent {
    #[serde(rename = "data", default)]
    data: Vec<RbData>,
}

#[derive(Debug, Deserialize)]
struct RbData {
    #[serde(rename = "value", default)]
    value: Option<RbValue>,
}

#[derive(Debug, Deserialize)]
struct RbValue {
    /// Present when the deadlock-graph is real child XML under <value>.
    #[serde(rename = "deadlock-graph", default)]
    deadlock_graph: Option<DeadlockGraph>,
    /// Present when <value> instead holds the graph as escaped text.
    #[serde(rename = "$text", default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeadlockGraph {
    #[serde(rename = "@victim", default)]
    victim: Option<String>,
    #[serde(rename = "process-list", default)]
    process_list: Option<ProcessList>,
    #[serde(rename = "resource-list", default)]
    resource_list: Option<ResourceList>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProcessList {
    #[serde(rename = "process", default)]
    process: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProcessInfo {
    #[serde(rename = "@id", default)]
    id: Option<String>,
    #[serde(rename = "@spid", default)]
    spid: Option<String>,
    #[serde(rename = "@status", default)]
    status: Option<String>,
    #[serde(rename = "@waitresource", default)]
    waitresource: Option<String>,
    #[serde(rename = "@waittime", default)]
    waittime: Option<String>,
    #[serde(rename = "@isolationlevel", default)]
    isolationlevel: Option<String>,
    #[serde(rename = "@lockMode", default)]
    lock_mode: Option<String>,
    #[serde(rename = "@clientapp", default)]
    clientapp: Option<String>,
    #[serde(rename = "@hostname", default)]
    hostname: Option<String>,
    #[serde(rename = "inputbuf", default)]
    inputbuf: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResourceList {
    #[serde(rename = "keylock", default)]
    keylock: Vec<LockNode>,
    #[serde(rename = "pagelock", default)]
    pagelock: Vec<LockNode>,
    #[serde(rename = "objectlock", default)]
    objectlock: Vec<LockNode>,
    #[serde(rename = "ridlock", default)]
    ridlock: Vec<LockNode>,
    #[serde(rename = "exchangeEvent", default)]
    exchange_event: Vec<LockNode>,
}

#[derive(Debug, Clone, Deserialize)]
struct LockNode {
    #[serde(rename = "@objectname", default)]
    objectname: Option<String>,
    #[serde(rename = "@dbid", default)]
    dbid: Option<String>,
    #[serde(rename = "@indexname", default)]
    indexname: Option<String>,
    #[serde(rename = "owner-list", default)]
    owner_list: Option<OwnerList>,
    #[serde(rename = "waiter-list", default)]
    waiter_list: Option<WaiterList>,
}

#[derive(Debug, Clone, Deserialize)]
struct OwnerList {
    #[serde(rename = "owner", default)]
    owner: Vec<LockParty>,
}

#[derive(Debug, Clone, Deserialize)]
struct WaiterList {
    #[serde(rename = "waiter", default)]
    waiter: Vec<LockParty>,
}

#[derive(Debug, Clone, Deserialize)]
struct LockParty {
    #[serde(rename = "@id", default)]
    id: Option<String>,
    #[serde(rename = "@mode", default)]
    mode: Option<String>,
}

// ===========================================================================
// Extracted, friendly context.
// ===========================================================================

#[derive(Debug, Clone, serde::Serialize)]
struct DeadlockProcess {
    id: String,
    spid: Option<String>,
    status: Option<String>,
    is_victim: bool,
    isolation_level: Option<String>,
    lock_mode: Option<String>,
    wait_resource: Option<String>,
    /// How long (ms, as reported by XE) this process waited before the cycle.
    wait_time: Option<String>,
    host: Option<String>,
    app: Option<String>,
    /// The captured input buffer (the statement). May be truncated by XE.
    sql: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct CycleEdge {
    owner: String,
    owner_mode: Option<String>,
    waiter: String,
    waiter_mode: Option<String>,
    object: Option<String>,
    /// Index name on the contended object, when the graph names it.
    index: Option<String>,
    /// Database id the lock resource lives in, when captured.
    dbid: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DeadlockContext {
    victims: Vec<String>,
    processes: Vec<DeadlockProcess>,
    /// Distinct object names touched by the locks (e.g. "dbo.Orders.PK_Orders").
    resources: Vec<String>,
    /// owner→waiter edges that reconstruct the circular wait.
    cycle_chain: Vec<CycleEdge>,
    /// True if any owner holds an exclusive/update lock while a reader waits —
    /// the classic reader/writer conflict that RCSI relieves.
    reader_writer_conflict: bool,
    /// True if an exchangeEvent participated — i.e. an intra-query PARALLEL
    /// deadlock, which is remediated via MAXDOP / cost-threshold, NOT indexes.
    parallel: bool,
}

// ===========================================================================
// Entry point.
// ===========================================================================

pub fn enrich(req: &IssueDetailReq, store: Option<&ReadStore>, window: TimeRange) -> Remediation {
    let recent = store.and_then(|s| match s.get_recent_deadlock(window) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "backend::health::enrichment", "deadlock read failed: {e}");
            None
        }
    });

    let Some((captured_at, blob)) = recent else {
        return generic(req, "No deadlock graph was captured in the window. Ensure sentinel is polling and the login has VIEW SERVER STATE so it can read the system_health ring buffer.");
    };

    let Some(graph) = extract_last_graph(&blob) else {
        tracing::warn!(target: "backend::health::enrichment", "deadlock XML parse yielded no graph; degrading");
        return generic(req, "A deadlock blob was captured but could not be parsed into a graph (the system_health XML format varies across SQL Server versions). Showing generic guidance; the raw blob is still in deadlock_capture for manual review.");
    };

    let ctx = extract_context(&graph);
    let age = relative_age(captured_at);
    let stale = (chrono::Utc::now() - captured_at).num_hours() > 24;

    let proc_count = ctx.processes.len();
    let victim_count = ctx.victims.len();
    let resources_str = if ctx.resources.is_empty() {
        "(resource names not captured)".to_string()
    } else {
        ctx.resources.join(", ")
    };
    let mut diagnosis = format!(
        "Deadlock cycle across {proc_count} process(es); {victim_count} victim(s) rolled back. \
         Resources: {resources_str}. Captured {age} ago."
    );
    if ctx.parallel {
        diagnosis.push_str(" An exchangeEvent participated — this is an intra-query PARALLEL deadlock, so the remedy family is MAXDOP / Cost Threshold for Parallelism, not indexing.");
    }
    if ctx.reader_writer_conflict {
        diagnosis.push_str(" A reader is blocked by a writer holding an X/U lock (the classic reader/writer conflict RCSI relieves).");
    }
    if stale {
        diagnosis.push_str(" CAVEAT: this graph is over 24h old — confirm it still recurs before acting.");
    }
    diagnosis.push_str(" Note: Extended Events can truncate or mis-attribute the captured statement — verify against the raw graph below.");

    let solutions = synthesize_solutions(&ctx, &req.conn.database);

    let supplemental = serde_json::to_value(&ctx).ok();

    Remediation {
        issue_id: req.issue_id.clone(),
        issue_kind: req.issue_kind.clone(),
        diagnosis,
        solution_steps: vec![
            RemediationStep::with_detail(
                "Identify the cycle from the parsed graph below",
                "Confirm the victim, the participating statements, and the owner→waiter lock chain before changing anything.",
            ),
            RemediationStep::with_detail(
                "Work the ranked ladder top-down",
                "Start with the safest, highest-likelihood option (a covering index or consistent lock order) before reaching for an isolation-level change.",
            ),
            RemediationStep::with_detail(
                "Add application-side retry-with-backoff as the universal baseline",
                "Deadlocks are non-fatal to design: catching error 1205 and retrying the transaction is always-safe insurance while the structural fix lands.",
            ),
        ],
        solutions,
        fix_sql: None,
        apply_safely: vec![
            "Investigate and apply off-peak — index builds and isolation changes touch live workloads.".to_string(),
            "Enable trace flag 1222 (or a dedicated xml_deadlock_report XE session) so future graphs are captured in full.".to_string(),
            "RCSI/SI needs tempdb headroom and adds row-versioning overhead — pre-test in staging, never toggle blind in prod.".to_string(),
            "Never reorder locks or rewrite a transaction without confirming the change preserves the original correctness/consistency.".to_string(),
        ],
        validate: vec![
            "Monitor sys.dm_tran_locks and the system_health session for NEW deadlocks over the next 24h.".to_string(),
            "Re-run the health scan — the deadlock_count signal for these resources should fall (the count is the KPI).".to_string(),
        ],
        rollback: vec![
            "Procedural fixes (lock order, shorter txn, retry) have no DDL to undo.".to_string(),
            "If you enabled RCSI: ALTER DATABASE [{db}] SET READ_COMMITTED_SNAPSHOT OFF; (do this only with no active connections).".replace("{db}", req.conn.database.as_deref().unwrap_or("<database>")),
            "If you added an index: DROP INDEX [<name>] ON <object>; — metadata-only, instant.".to_string(),
        ],
        impact: "Eliminating victim rollbacks typically lifts throughput 5–50% and removes user-facing error 1205 retries. Cost: any added index carries write/storage overhead (shown per-option); RCSI adds tempdb + version-store pressure. Confidence: medium — backed by the parsed cycle, but XE may have truncated a statement.".to_string(),
        supplemental,
    }
}

// ===========================================================================
// Parse helpers.
// ===========================================================================

/// Pull the LAST (most-recent) deadlock-graph out of the ring-buffer blob.
/// Handles both child-XML and escaped-text `<value>` bodies, and also the case
/// where the blob is itself a single bare `<deadlock-graph>`.
fn extract_last_graph(blob: &str) -> Option<DeadlockGraph> {
    // Fast path: the blob is a bare deadlock-graph.
    if let Ok(g) = quick_xml::de::from_str::<DeadlockGraph>(blob) {
        if g.process_list.is_some() || g.victim.is_some() {
            return Some(g);
        }
    }

    // Normal path: a ring-buffer target with N events.
    let target: RingBufferTarget = quick_xml::de::from_str(blob).ok()?;
    let mut last: Option<DeadlockGraph> = None;
    for event in &target.events {
        for data in &event.data {
            let Some(value) = &data.value else { continue };
            // (a) graph as real child XML.
            if let Some(g) = &value.deadlock_graph {
                last = Some(g.clone());
                continue;
            }
            // (b) graph as escaped text — re-parse the inner string.
            if let Some(text) = &value.text {
                let trimmed = text.trim();
                if trimmed.contains("deadlock-graph") {
                    if let Ok(g) = quick_xml::de::from_str::<DeadlockGraph>(trimmed) {
                        last = Some(g);
                    }
                }
            }
        }
    }
    last
}

fn extract_context(graph: &DeadlockGraph) -> DeadlockContext {
    let mut processes = Vec::new();
    let mut victims = Vec::new();

    let victim_attr = graph.victim.clone();
    if let Some(pl) = &graph.process_list {
        for p in &pl.process {
            let id = p.id.clone().unwrap_or_default();
            let status_is_victim = p
                .status
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("victim"))
                .unwrap_or(false);
            // The graph's @victim attribute references a process id; honor both
            // signals (status="victim" OR the top-level @victim pointer).
            let pointer_victim = victim_attr
                .as_deref()
                .map(|v| !id.is_empty() && (v == id || v.ends_with(&id)))
                .unwrap_or(false);
            let is_victim = status_is_victim || pointer_victim;
            if is_victim && !id.is_empty() {
                victims.push(id.clone());
            }
            processes.push(DeadlockProcess {
                id,
                spid: p.spid.clone(),
                status: p.status.clone(),
                is_victim,
                isolation_level: p.isolationlevel.clone(),
                lock_mode: p.lock_mode.clone(),
                wait_resource: p.waitresource.clone(),
                wait_time: p.waittime.clone(),
                host: p.hostname.clone(),
                app: p.clientapp.clone(),
                sql: p.inputbuf.as_ref().map(|s| clean_sql(s)),
            });
        }
    }

    let mut resources: Vec<String> = Vec::new();
    let mut cycle_chain: Vec<CycleEdge> = Vec::new();
    let mut reader_writer_conflict = false;
    let mut parallel = false;

    if let Some(rl) = &graph.resource_list {
        if !rl.exchange_event.is_empty() {
            parallel = true;
        }
        for node in rl
            .keylock
            .iter()
            .chain(&rl.pagelock)
            .chain(&rl.objectlock)
            .chain(&rl.ridlock)
            .chain(&rl.exchange_event)
        {
            if let Some(obj) = &node.objectname {
                if !obj.is_empty() && !resources.contains(obj) {
                    resources.push(obj.clone());
                }
            }
            let owners = node.owner_list.as_ref().map(|o| o.owner.as_slice()).unwrap_or(&[]);
            let waiters = node.waiter_list.as_ref().map(|w| w.waiter.as_slice()).unwrap_or(&[]);
            for owner in owners {
                for waiter in waiters {
                    let owner_mode = owner.mode.clone();
                    let waiter_mode = waiter.mode.clone();
                    if is_write_mode(owner_mode.as_deref()) && is_read_mode(waiter_mode.as_deref()) {
                        reader_writer_conflict = true;
                    }
                    cycle_chain.push(CycleEdge {
                        owner: owner.id.clone().unwrap_or_default(),
                        owner_mode,
                        waiter: waiter.id.clone().unwrap_or_default(),
                        waiter_mode,
                        object: node.objectname.clone(),
                        index: node.indexname.clone(),
                        dbid: node.dbid.clone(),
                    });
                }
            }
        }
    }

    DeadlockContext {
        victims,
        processes,
        resources,
        cycle_chain,
        reader_writer_conflict,
        parallel,
    }
}

fn is_write_mode(mode: Option<&str>) -> bool {
    matches!(mode, Some(m) if m.starts_with('X') || m.starts_with('U') || m.contains("IX"))
}

fn is_read_mode(mode: Option<&str>) -> bool {
    matches!(mode, Some(m) if m.starts_with('S') || m.starts_with("RangeS"))
}

/// Tidy an inputbuf: collapse runs of whitespace so the diagnosis prose reads
/// cleanly, and cap the length (XE already truncates, but be defensive).
fn clean_sql(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > 1000 {
        format!("{}…", &collapsed[..1000])
    } else {
        collapsed
    }
}

/// Heuristic: does any participant statement look scan-prone (a DML/SELECT that
/// would benefit from a covering index)?
fn looks_scan_prone(ctx: &DeadlockContext) -> bool {
    ctx.processes.iter().any(|p| {
        p.sql
            .as_deref()
            .map(|s| {
                let up = s.to_uppercase();
                up.contains("SELECT") || up.contains("UPDATE") || up.contains("DELETE")
            })
            .unwrap_or(false)
    })
}

fn chain_summary(ctx: &DeadlockContext) -> String {
    if ctx.cycle_chain.is_empty() {
        return "owner→waiter chain not captured".to_string();
    }
    ctx.cycle_chain
        .iter()
        .take(6)
        .map(|e| {
            format!(
                "{}({}) -> {}({})",
                e.owner,
                e.owner_mode.as_deref().unwrap_or("?"),
                e.waiter,
                e.waiter_mode.as_deref().unwrap_or("?"),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn participant_sql_list(ctx: &DeadlockContext) -> String {
    let sqls: Vec<String> = ctx
        .processes
        .iter()
        .filter_map(|p| p.sql.clone())
        .filter(|s| !s.is_empty())
        .collect();
    if sqls.is_empty() {
        "(participant statements not captured)".to_string()
    } else {
        sqls.join(" | ")
    }
}

// ===========================================================================
// Ladder synthesis.
// ===========================================================================

fn synthesize_solutions(ctx: &DeadlockContext, db: &Option<String>) -> Vec<SolutionOption> {
    // Parallel (intra-query) deadlocks have a completely different remedy
    // family — surface that first and route away from index/query changes.
    if ctx.parallel {
        return parallel_ladder(ctx);
    }

    let mut ladder = Vec::new();
    let db_name = db.as_deref().unwrap_or("<database>");
    let first_resource = ctx.resources.first().cloned().unwrap_or_else(|| "<schema>.<table>".to_string());
    let victim_sql = ctx
        .processes
        .iter()
        .find(|p| p.is_victim)
        .and_then(|p| p.sql.clone())
        .or_else(|| ctx.processes.iter().find_map(|p| p.sql.clone()))
        .unwrap_or_else(|| "(victim statement not captured by Extended Events)".to_string());

    let mut rank = 0u32;

    // rank0 — INDEX, only when a participant looks scan-prone.
    if looks_scan_prone(ctx) {
        let sql_fix = format!(
            "-- Covering index to shorten the lock window on {obj}.\n\
             -- Seeded from the deadlock; FILL IN the predicate/JOIN/ORDER-BY columns\n\
             -- as key columns and the SELECT-list columns as INCLUDE.\n\
             -- Victim statement (verify, XE may truncate):\n\
             --   {sql}\n\
             CREATE NONCLUSTERED INDEX IX_dl_{idx} ON {obj} (/* key cols */)\n\
                 INCLUDE (/* covering cols */)\n\
                 WITH (ONLINE = ON); -- ONLINE requires Enterprise/Developer 2016 SP2+",
            obj = first_resource,
            sql = victim_sql,
            idx = sanitize_ident(&first_resource),
        );
        ladder.push(SolutionOption {
            rank,
            category: "index".to_string(),
            description: "Add a covering index so the scan-driven side seeks instead of escalating locks across the table.".to_string(),
            sql_fix: Some(sql_fix),
            risk_level: "safe".to_string(),
            estimated_impact: "Eliminates the deadlock if it is lock-escalation on a scan; shortens the held-lock window.".to_string(),
            notes: format!(
                "BENEFIT vs COST: a covering index removes the scan but adds write amplification on every INSERT/UPDATE/DELETE plus index storage — size the key narrowly and only INCLUDE columns the query reads. Check existing indexes on {first_resource} first (ADVISE/INDEX workspace) and prefer WIDENING an existing one over adding a new overlapping index."
            ),
        });
        rank += 1;
    }

    // rank1 — LOCK ORDER (procedural).
    ladder.push(SolutionOption {
        rank,
        category: "lock-order".to_string(),
        description: "Enforce a consistent object access order across all transactions to break the cycle.".to_string(),
        sql_fix: None,
        risk_level: "moderate".to_string(),
        estimated_impact: "Removes the circular wait directly when the deadlock is an ordering problem.".to_string(),
        notes: format!(
            "Observed owner→waiter chain: {}. If transaction A locks {} in one order and B locks them in the reverse order, make every transaction acquire them in the SAME order. Touched resources: {}.",
            chain_summary(ctx),
            ctx.resources.first().map(|_| ctx.resources.join(" then ")).unwrap_or_else(|| "the objects".to_string()),
            if ctx.resources.is_empty() { "(not captured)".to_string() } else { ctx.resources.join(", ") },
        ),
    });
    rank += 1;

    // rank2 — ISOLATION (RCSI), strongly indicated for reader/writer conflicts.
    let rcsi_impact = if ctx.reader_writer_conflict {
        "Likely to help: a reader/writer conflict was detected in this cycle — RCSI lets readers see a row version instead of blocking on the writer's X/U lock."
    } else {
        "May help if reads are blocking on writers; less relevant for pure writer/writer cycles."
    };
    ladder.push(SolutionOption {
        rank,
        category: "isolation".to_string(),
        description: "Enable Read Committed Snapshot Isolation (RCSI) so readers stop blocking on writers.".to_string(),
        sql_fix: Some(format!(
            "-- Run with NO active connections to [{db_name}] (briefly exclusive).\nALTER DATABASE [{db_name}] SET READ_COMMITTED_SNAPSHOT ON;"
        )),
        risk_level: "moderate".to_string(),
        estimated_impact: rcsi_impact.to_string(),
        notes: format!(
            "BENEFIT vs COST: RCSI removes most reader/writer deadlocks but COSTS tempdb space + version-store maintenance and adds 14 bytes per row over time. Reader/writer conflict detected in this graph: {}. Pre-test in staging and watch tempdb.",
            if ctx.reader_writer_conflict { "YES" } else { "no (so this is a weaker recommendation here)" }
        ),
    });
    rank += 1;

    // rank3 — TXN SCOPE (procedural).
    ladder.push(SolutionOption {
        rank,
        category: "txn-scope".to_string(),
        description: "Shorten the transaction: move I/O, external calls, and user think-time OUT of the open transaction.".to_string(),
        sql_fix: None,
        risk_level: "safe".to_string(),
        estimated_impact: "Reduces how long locks are held, shrinking the window in which a cycle can form.".to_string(),
        notes: format!(
            "Participant statements (verify; XE may truncate): {}. If any holds locks while waiting on app logic, a web service, or a file, pull that work before BEGIN TRAN or after COMMIT.",
            participant_sql_list(ctx)
        ),
    });
    rank += 1;

    // rank4 — QUERY REWRITE (risky).
    ladder.push(SolutionOption {
        rank,
        category: "query-rewrite".to_string(),
        description: "Reduce hot-key contention: chunk large batch DML and avoid updating the same hot rows from many sessions at once.".to_string(),
        sql_fix: None,
        risk_level: "risky".to_string(),
        estimated_impact: "Helps hot-key / batch-on-batch deadlocks; requires understanding the workload and may change behavior.".to_string(),
        notes: "Smaller batches acquire fewer locks per statement; ordering updates by key and processing in consistent chunks reduces the chance two sessions interleave into a cycle. Test correctness carefully.".to_string(),
    });

    ladder
}

fn parallel_ladder(ctx: &DeadlockContext) -> Vec<SolutionOption> {
    vec![
        SolutionOption {
            rank: 0,
            category: "parallelism".to_string(),
            description: "Raise Cost Threshold for Parallelism so trivially-cheap plans stop going parallel.".to_string(),
            sql_fix: Some(
                "-- Default is 5 (far too low for modern hardware). 50 is a common starting point.\nEXEC sp_configure 'cost threshold for parallelism', 50;\nRECONFIGURE;".to_string(),
            ),
            risk_level: "moderate".to_string(),
            estimated_impact: "Removes intra-query exchange deadlocks for the many small queries that should never have parallelized.".to_string(),
            notes: format!(
                "This is a PARALLEL (intra-query exchangeEvent) deadlock — index/query changes do not apply. Observed chain: {}. Tune at the instance level and re-measure.",
                chain_summary(ctx)
            ),
        },
        SolutionOption {
            rank: 1,
            category: "parallelism".to_string(),
            description: "Lower MAXDOP for the affected workload (instance, database scope, or query hint).".to_string(),
            sql_fix: Some(
                "-- Instance-level example; prefer Database Scoped Configuration or OPTION(MAXDOP n) for surgical scope.\nEXEC sp_configure 'max degree of parallelism', 8;\nRECONFIGURE;".to_string(),
            ),
            risk_level: "moderate".to_string(),
            estimated_impact: "Reduces or eliminates exchange-operator deadlocks; may slow genuinely parallel-friendly analytic queries.".to_string(),
            notes: "BENEFIT vs COST: capping MAXDOP cuts parallel deadlocks but can lengthen large scans/aggregations — scope it as narrowly as possible (query hint > database scoped > instance).".to_string(),
        },
    ]
}

fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// ===========================================================================
// Graceful fallback (no graph / parse failure).
// ===========================================================================

fn generic(req: &IssueDetailReq, why: &str) -> Remediation {
    let db_name = req.conn.database.as_deref().unwrap_or("<database>");
    Remediation {
        issue_id: req.issue_id.clone(),
        issue_kind: req.issue_kind.clone(),
        diagnosis: format!(
            "{why} Deadlocks roll back at least one victim transaction; the safe, evidence-backed remedies below apply to the common cycle shapes."
        ),
        solution_steps: vec![
            RemediationStep::with_detail(
                "Capture a fuller deadlock graph",
                "Enable trace flag 1222 or a dedicated sqlserver.xml_deadlock_report XE session, then re-open this detail to get the cycle-specific ladder.",
            ),
            RemediationStep::new("Add application-side retry-with-backoff on error 1205 as the universal baseline."),
        ],
        solutions: vec![
            SolutionOption {
                rank: 0,
                category: "lock-order".to_string(),
                description: "Enforce a consistent object access order across transactions.".to_string(),
                sql_fix: None,
                risk_level: "moderate".to_string(),
                estimated_impact: "Breaks ordering-driven cycles directly.".to_string(),
                notes: "If different transactions touch the same tables in different orders, make them all use one order.".to_string(),
            },
            SolutionOption {
                rank: 1,
                category: "index".to_string(),
                description: "Add covering indexes on scan-heavy participants to shorten the lock window.".to_string(),
                sql_fix: None,
                risk_level: "safe".to_string(),
                estimated_impact: "Seeks instead of scans → fewer/narrower locks → fewer cycles.".to_string(),
                notes: "BENEFIT vs COST: a covering index adds write amplification + storage; check existing indexes first and prefer widening one.".to_string(),
            },
            SolutionOption {
                rank: 2,
                category: "isolation".to_string(),
                description: "Consider RCSI if readers are blocking on writers.".to_string(),
                sql_fix: Some(format!("ALTER DATABASE [{db_name}] SET READ_COMMITTED_SNAPSHOT ON;")),
                risk_level: "moderate".to_string(),
                estimated_impact: "Removes most reader/writer deadlocks.".to_string(),
                notes: "COST: tempdb version store + ~14 bytes/row. Pre-test in staging; you MIGHT need this — confirm a reader/writer conflict first.".to_string(),
            },
        ],
        fix_sql: None,
        apply_safely: vec![
            "Investigate off-peak.".to_string(),
            "Enable trace flag 1222 for full future graphs.".to_string(),
            "RCSI needs tempdb headroom — pre-test in staging.".to_string(),
        ],
        validate: vec![
            "Monitor sys.dm_tran_locks / system_health for new deadlocks over 24h.".to_string(),
            "Re-run the health scan; deadlock_count should fall.".to_string(),
        ],
        rollback: vec![
            "No DDL for procedural fixes.".to_string(),
            format!("If RCSI was enabled: ALTER DATABASE [{db_name}] SET READ_COMMITTED_SNAPSHOT OFF;"),
        ],
        impact: "Eliminating victim rollbacks lifts throughput 5–50%. Confidence: low until a real graph is captured.".to_string(),
        supplemental: None,
    }
}
