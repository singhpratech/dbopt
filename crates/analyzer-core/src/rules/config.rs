//! Database / server configuration script smells (offline, static review of
//! admin T-SQL — community best-practice scripts territory without a live connection).
//!
//! Every rule here works purely off the token stream, so it must be careful:
//! the tokenizer already classifies comments (`TokKind::Comment`), string
//! literals (`TokKind::String`) and bracket-quoted identifiers (a single
//! `TokKind::Word` whose text begins with `[`). We only ever match on real
//! `Word` / `Number` / `Punct` tokens, so `-- AUTO_SHRINK ON` in a comment or
//! `'DBCC SHRINKFILE'` inside a string never fires. When a match is ambiguous
//! we drop it rather than risk a false positive.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

/// Next non-comment token index at or after `from`.
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
        k += 1;
    }
    k
}

/// Strip a string literal down to its inner text (drops the surrounding quotes
/// and an optional leading `N` unicode prefix). Used for `sp_configure 'name'`.
fn string_inner(t: &Token<'_>) -> String {
    t.text
        .trim_start_matches(|c| c == 'N' || c == 'n')
        .trim_matches('\'')
        .to_string()
}

// ---------------------------------------------------------------------------
// (a) ALTER DATABASE ... SET AUTO_SHRINK ON
// (b) ALTER DATABASE ... SET AUTO_CLOSE ON
// (d) ALTER DATABASE ... SET PAGE_VERIFY NONE | TORN_PAGE_DETECTION
// (f) ALTER DATABASE ... SET RECOVERY SIMPLE
// All four ride the same `ALTER DATABASE ... SET <option> <value>` walk.
// ---------------------------------------------------------------------------

/// Returns true if `t` begins an `ALTER DATABASE` statement. We only treat the
/// keyword pair as a real statement head (not a quoted identifier or comment).
fn is_alter_database(tokens: &[Token<'_>], i: usize) -> bool {
    if !is_word(&tokens[i], "ALTER") {
        return false;
    }
    let j = skip_comments(tokens, i + 1);
    j < tokens.len() && is_word(&tokens[j], "DATABASE")
}

/// `ALTER DATABASE ... SET AUTO_SHRINK ON` — auto-shrink is a documented
/// anti-pattern: it churns pages, massively fragments indexes, and the shrink +
/// regrow cycle hammers I/O. Microsoft recommends it stay OFF.
pub fn auto_shrink_on(ctx: &RuleCtx) -> Vec<Finding> {
    set_option_on(
        ctx,
        "AUTO_SHRINK",
        "config.auto_shrink_on",
        Severity::Warning,
        "ALTER DATABASE … SET AUTO_SHRINK ON — auto-shrink continuously shrinks the data file, then it grows back, churning I/O and severely fragmenting every index.",
        "Turn it off: `ALTER DATABASE [YourDb] SET AUTO_SHRINK OFF;`. Reclaim space deliberately with a one-time, manual shrink during a maintenance window, then rebuild indexes — never on a schedule.",
    )
}

/// `ALTER DATABASE ... SET AUTO_CLOSE ON` — closes the DB and frees resources
/// when the last user disconnects; the next connection pays a full open + cache
/// warm-up. A relic of MSDE / desktop editions, harmful on any server DB.
pub fn auto_close_on(ctx: &RuleCtx) -> Vec<Finding> {
    set_option_on(
        ctx,
        "AUTO_CLOSE",
        "config.auto_close_on",
        Severity::Warning,
        "ALTER DATABASE … SET AUTO_CLOSE ON — the database is shut down when the last session disconnects, so the next connection pays a cold-start (file open + plan/cache rebuild) penalty.",
        "Turn it off: `ALTER DATABASE [YourDb] SET AUTO_CLOSE OFF;`. AUTO_CLOSE only makes sense on resource-starved desktop installs, never on a server database.",
    )
}

/// Shared `ALTER DATABASE ... SET <option> ON` matcher. Walks from the ALTER
/// token to a top-level `SET`, then expects `<option> ON`. `ON` is verified to
/// avoid firing on `SET AUTO_SHRINK OFF`.
fn set_option_on(
    ctx: &RuleCtx,
    option: &str,
    rule_id: &str,
    sev: Severity,
    msg: &str,
    rec: &str,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_alter_database(tokens, i) {
            i += 1;
            continue;
        }
        // Walk to the SET ... <option> pair before the next statement terminator.
        let mut k = skip_comments(tokens, i + 2);
        while k < tokens.len() && tokens[k].text != ";" {
            if is_word(&tokens[k], "SET") {
                let opt = skip_comments(tokens, k + 1);
                if opt < tokens.len() && is_word(&tokens[opt], option) {
                    let val = skip_comments(tokens, opt + 1);
                    if val < tokens.len() && is_word(&tokens[val], "ON") {
                        out.push(finding(
                            rule_id,
                            sev,
                            msg.to_string(),
                            Some(make_loc(&tokens[opt])),
                            Some(rec.to_string()),
                        ));
                    }
                }
                break;
            }
            k += 1;
        }
        i = k.max(i + 1);
    }
    out
}

/// `ALTER DATABASE ... SET PAGE_VERIFY NONE | TORN_PAGE_DETECTION` — anything
/// other than CHECKSUM means corrupt pages can go undetected. CHECKSUM is the
/// modern default; NONE is the worst, TORN_PAGE_DETECTION is the weak legacy
/// option.
pub fn page_verify_not_checksum(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_alter_database(tokens, i) {
            i += 1;
            continue;
        }
        let mut k = skip_comments(tokens, i + 2);
        while k < tokens.len() && tokens[k].text != ";" {
            if is_word(&tokens[k], "SET") {
                let opt = skip_comments(tokens, k + 1);
                if opt < tokens.len() && is_word(&tokens[opt], "PAGE_VERIFY") {
                    let val = skip_comments(tokens, opt + 1);
                    if val < tokens.len() {
                        let bad = if is_word(&tokens[val], "NONE") {
                            Some("NONE")
                        } else if is_word(&tokens[val], "TORN_PAGE_DETECTION") {
                            Some("TORN_PAGE_DETECTION")
                        } else {
                            None
                        };
                        if let Some(mode) = bad {
                            out.push(finding(
                                "config.page_verify_not_checksum",
                                Severity::Warning,
                                format!("ALTER DATABASE … SET PAGE_VERIFY {} — with anything other than CHECKSUM, torn pages and storage corruption can go undetected until you read the damaged page.", mode),
                                Some(make_loc(&tokens[val])),
                                Some("Use CHECKSUM (the modern default): `ALTER DATABASE [YourDb] SET PAGE_VERIFY CHECKSUM;`. Existing pages get a checksum the next time they are written; a full rewrite (index rebuild / DBCC) accelerates coverage.".into()),
                            ));
                        }
                    }
                }
                break;
            }
            k += 1;
        }
        i = k.max(i + 1);
    }
    out
}

/// `ALTER DATABASE ... SET RECOVERY SIMPLE` — Info/caveated. SIMPLE recovery is
/// correct for dev/scratch DBs but on production it means no point-in-time
/// restore (you lose everything since the last full/diff backup). We can't know
/// the DB's role offline, so this is advisory only.
pub fn recovery_simple(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_alter_database(tokens, i) {
            i += 1;
            continue;
        }
        let mut k = skip_comments(tokens, i + 2);
        while k < tokens.len() && tokens[k].text != ";" {
            if is_word(&tokens[k], "SET") {
                let opt = skip_comments(tokens, k + 1);
                if opt < tokens.len() && is_word(&tokens[opt], "RECOVERY") {
                    let val = skip_comments(tokens, opt + 1);
                    if val < tokens.len() && is_word(&tokens[val], "SIMPLE") {
                        out.push(finding(
                            "config.recovery_model_simple",
                            Severity::Info,
                            "ALTER DATABASE … SET RECOVERY SIMPLE — SIMPLE recovery discards the log on each checkpoint, so point-in-time / log-shipping restore is impossible. Confirm this database is not production.",
                            Some(make_loc(&tokens[val])),
                            Some("If this is a dev/scratch/staging database, SIMPLE is fine. For any database that needs point-in-time recovery use FULL and take regular log backups: `ALTER DATABASE [YourDb] SET RECOVERY FULL;` then schedule `BACKUP LOG`.".into()),
                        ));
                    }
                }
                break;
            }
            k += 1;
        }
        i = k.max(i + 1);
    }
    out
}

// ---------------------------------------------------------------------------
// (c) DBCC SHRINKDATABASE / DBCC SHRINKFILE
// ---------------------------------------------------------------------------

/// `DBCC SHRINKDATABASE` / `DBCC SHRINKFILE` — manual shrink fragments every
/// index it touches (it moves pages from the end of the file to the front,
/// reversing their logical order) and the file usually regrows immediately. A
/// recurring shrink is the single most common self-inflicted performance wound.
pub fn dbcc_shrink(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "DBCC") {
            continue;
        }
        let j = skip_comments(tokens, i + 1);
        if j >= tokens.len() {
            continue;
        }
        let cmd = if is_word(&tokens[j], "SHRINKDATABASE") {
            Some("SHRINKDATABASE")
        } else if is_word(&tokens[j], "SHRINKFILE") {
            Some("SHRINKFILE")
        } else {
            None
        };
        if let Some(cmd) = cmd {
            // Don't flag the legitimate maintenance forms: SHRINKFILE (..., EMPTYFILE)
            // (emptying a file before removing it) and (..., TRUNCATEONLY) (releasing
            // free space at the end of the file without moving pages → no
            // fragmentation). Scan this DBCC statement's arguments for either option.
            let mut k = j + 1;
            let mut benign = false;
            while k < tokens.len() && tokens[k].text != ";" && !is_word(&tokens[k], "DBCC") {
                if is_word(&tokens[k], "EMPTYFILE") || is_word(&tokens[k], "TRUNCATEONLY") {
                    benign = true;
                    break;
                }
                k += 1;
            }
            if benign {
                continue;
            }
            out.push(finding(
                "config.dbcc_shrink",
                Severity::Warning,
                format!("DBCC {} fragments every index in the shrunk file and the file typically regrows right after, so the operation is pure churn — never schedule it.", cmd),
                Some(make_loc(&tokens[j])),
                Some("Avoid routine shrinking. If you must reclaim a one-off block of space after archiving, shrink once in a maintenance window then rebuild indexes: `DBCC SHRINKFILE (N'YourDb', <target_mb>); ALTER INDEX ALL ON <table> REBUILD;`. Right-size the files and let them grow instead of shrinking on a schedule.".into()),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// (g) DBCC TRACEON ( NNNN, -1 ) — global trace flags
// ---------------------------------------------------------------------------

/// `DBCC TRACEON ( <flag>, -1 )` — the `-1` makes the trace flag global
/// (instance-wide for every session) instead of session-scoped. Global trace
/// flags change optimizer/engine behavior server-wide and are easy to forget;
/// many should be startup `-T` flags (so they survive restart) or, on modern
/// SQL Server, replaced by a documented database-scoped configuration.
///
/// We deliberately skip flags 1211 / 1224 — those are already covered with a
/// more specific message by `locking::trace_flag_lock_escalation_disabled`, so
/// excluding them avoids two findings on the same token.
pub fn dbcc_traceon_global(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "DBCC") {
            i += 1;
            continue;
        }
        let j = skip_comments(tokens, i + 1);
        if j >= tokens.len() || !is_word(&tokens[j], "TRACEON") {
            i += 1;
            continue;
        }
        let lp = skip_comments(tokens, j + 1);
        if lp >= tokens.len() || tokens[lp].text != "(" {
            i = j + 1;
            continue;
        }
        // Collect the comma-separated args; detect a `-1` (global) and the first
        // flag number for the message + location.
        let mut k = lp + 1;
        let mut depth = 1i32;
        let mut flag_tok: Option<usize> = None;
        let mut is_global = false;
        let mut prev_was_minus = false;
        while k < tokens.len() && depth > 0 {
            let tk = &tokens[k];
            if tk.text == "(" {
                depth += 1;
            } else if tk.text == ")" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if tk.kind == TokKind::Number {
                if prev_was_minus && tk.text == "1" {
                    is_global = true;
                } else if flag_tok.is_none() {
                    flag_tok = Some(k);
                }
            }
            prev_was_minus = tk.text == "-";
            k += 1;
        }
        if is_global {
            if let Some(ft) = flag_tok {
                let flag = tokens[ft].text;
                // 1211 / 1224 owned by the locking pack — don't double-report.
                if flag != "1211" && flag != "1224" {
                    out.push(finding(
                        "config.dbcc_traceon_global",
                        Severity::Warning,
                        format!("DBCC TRACEON ({}, -1) enables trace flag {} globally for every session on the instance — a server-wide behavior change that is easy to lose track of.", flag, flag),
                        Some(make_loc(&tokens[ft])),
                        Some(format!("If the flag must persist, set it as a startup parameter (`-T{}`) so it survives a restart, and document why. On modern SQL Server prefer a supported equivalent (e.g. database-scoped configuration / `ALTER DATABASE SCOPED CONFIGURATION`) over a global trace flag where one exists.", flag)),
                    ));
                }
            }
        }
        i = if k > i { k } else { i + 1 };
    }
    out
}

// ---------------------------------------------------------------------------
// (e) sp_configure 'max degree of parallelism', 0
//     sp_configure 'cost threshold for parallelism', 5
// ---------------------------------------------------------------------------

/// `sp_configure` with a known-bad default value. Two cases:
///   * `'max degree of parallelism', 0` — MAXDOP 0 lets a single query fan out
///     across every scheduler, which on a large box (many cores / NUMA) causes
///     excessive CXPACKET / CXCONSUMER waits.
///   * `'cost threshold for parallelism', 5` — the 1990s default of 5 sends
///     trivial queries parallel; modern guidance is to raise it (commonly 50).
///
/// We require the literal value token to match (0 / 5) so we never fire when the
/// admin is already setting a sane value. The setting name comes from a string
/// literal, so quoted names and comments are inherently safe.
pub fn sp_configure_known_bad(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("sp_configure")) {
            continue;
        }
        // sp_configure [ N ]'<name>' , <value>
        let name_idx = skip_comments(tokens, i + 1);
        if name_idx >= tokens.len() || tokens[name_idx].kind != TokKind::String {
            continue;
        }
        let name = string_inner(&tokens[name_idx]).to_ascii_lowercase();
        let name = name.trim().to_string();

        // Comma then the numeric value (allow an optional sign).
        let comma = skip_comments(tokens, name_idx + 1);
        if comma >= tokens.len() || tokens[comma].text != "," {
            continue;
        }
        let mut val_idx = skip_comments(tokens, comma + 1);
        if val_idx < tokens.len() && tokens[val_idx].text == "-" {
            val_idx = skip_comments(tokens, val_idx + 1);
        }
        if val_idx >= tokens.len() || tokens[val_idx].kind != TokKind::Number {
            continue;
        }
        let val = tokens[val_idx].text;

        if name == "max degree of parallelism" && val == "0" {
            out.push(finding(
                "config.maxdop_zero",
                Severity::Warning,
                "sp_configure 'max degree of parallelism', 0 — MAXDOP 0 lets one query use every scheduler on the box. On a large / multi-NUMA server this drives heavy CXPACKET / CXCONSUMER waits.",
                Some(make_loc(&tokens[val_idx])),
                Some("Cap MAXDOP. A common starting point: number of cores per NUMA node, up to 8. e.g. `EXEC sp_configure 'max degree of parallelism', 8; RECONFIGURE;`. Validate against your core/NUMA layout and workload.".into()),
            ));
        } else if name == "cost threshold for parallelism" && val == "5" {
            out.push(finding(
                "config.cost_threshold_default",
                Severity::Warning,
                "sp_configure 'cost threshold for parallelism', 5 — the legacy default of 5 pushes even trivial queries into parallel plans, multiplying scheduling overhead.",
                Some(make_loc(&tokens[val_idx])),
                Some("Raise it well above the 1990s default of 5 (commonly 50): `EXEC sp_configure 'cost threshold for parallelism', 50; RECONFIGURE;`. Tune from your plan-cache cost distribution.".into()),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    /// Build a ctx for `sql` and run a single rule fn, returning its findings.
    fn run(rule: super::super::RuleFn, sql: &str) -> Vec<Finding> {
        let tokens = tokenize(sql);
        let ctx = RuleCtx {
            src: sql,
            tokens: &tokens,
            server_version: Some(2025),
            engine: Engine::SqlServer,
        };
        rule(&ctx)
    }

    fn fires(rule: super::super::RuleFn, sql: &str, id: &str) -> bool {
        run(rule, sql).iter().any(|f| f.rule.0 == id && f.location.is_some())
    }

    // ---- (a) AUTO_SHRINK ----------------------------------------------------
    #[test]
    fn auto_shrink_on_fires() {
        assert!(fires(
            auto_shrink_on,
            "ALTER DATABASE [Sales] SET AUTO_SHRINK ON;",
            "config.auto_shrink_on"
        ));
    }
    #[test]
    fn auto_shrink_off_does_not_fire() {
        assert!(run(auto_shrink_on, "ALTER DATABASE [Sales] SET AUTO_SHRINK OFF;").is_empty());
    }
    #[test]
    fn auto_shrink_in_comment_does_not_fire() {
        assert!(run(
            auto_shrink_on,
            "-- ALTER DATABASE Sales SET AUTO_SHRINK ON\nSELECT 1;"
        )
        .is_empty());
    }
    #[test]
    fn auto_shrink_in_string_does_not_fire() {
        assert!(run(
            auto_shrink_on,
            "PRINT 'ALTER DATABASE Sales SET AUTO_SHRINK ON';"
        )
        .is_empty());
    }

    // ---- (b) AUTO_CLOSE -----------------------------------------------------
    #[test]
    fn auto_close_on_fires() {
        assert!(fires(
            auto_close_on,
            "ALTER DATABASE Sales SET AUTO_CLOSE ON;",
            "config.auto_close_on"
        ));
    }
    #[test]
    fn auto_close_off_does_not_fire() {
        assert!(run(auto_close_on, "ALTER DATABASE Sales SET AUTO_CLOSE OFF;").is_empty());
    }

    // ---- (c) DBCC SHRINK ----------------------------------------------------
    #[test]
    fn dbcc_shrinkdatabase_fires() {
        assert!(fires(
            dbcc_shrink,
            "DBCC SHRINKDATABASE (Sales, 10);",
            "config.dbcc_shrink"
        ));
    }
    #[test]
    fn dbcc_shrinkfile_fires() {
        assert!(fires(
            dbcc_shrink,
            "DBCC SHRINKFILE (Sales_log, 1);",
            "config.dbcc_shrink"
        ));
    }
    #[test]
    fn dbcc_checkdb_does_not_fire() {
        // CHECKDB is healthy maintenance — must not be flagged as a shrink.
        assert!(run(dbcc_shrink, "DBCC CHECKDB (Sales);").is_empty());
    }
    #[test]
    fn dbcc_shrink_in_comment_does_not_fire() {
        assert!(run(dbcc_shrink, "/* DBCC SHRINKFILE (x,1) */ SELECT 1;").is_empty());
    }

    // ---- (d) PAGE_VERIFY ----------------------------------------------------
    #[test]
    fn page_verify_none_fires() {
        assert!(fires(
            page_verify_not_checksum,
            "ALTER DATABASE Sales SET PAGE_VERIFY NONE;",
            "config.page_verify_not_checksum"
        ));
    }
    #[test]
    fn page_verify_torn_page_fires() {
        assert!(fires(
            page_verify_not_checksum,
            "ALTER DATABASE Sales SET PAGE_VERIFY TORN_PAGE_DETECTION;",
            "config.page_verify_not_checksum"
        ));
    }
    #[test]
    fn page_verify_checksum_does_not_fire() {
        assert!(run(
            page_verify_not_checksum,
            "ALTER DATABASE Sales SET PAGE_VERIFY CHECKSUM;"
        )
        .is_empty());
    }

    // ---- (e) sp_configure ---------------------------------------------------
    #[test]
    fn maxdop_zero_fires() {
        assert!(fires(
            sp_configure_known_bad,
            "EXEC sp_configure 'max degree of parallelism', 0;",
            "config.maxdop_zero"
        ));
    }
    #[test]
    fn maxdop_eight_does_not_fire() {
        assert!(run(
            sp_configure_known_bad,
            "EXEC sp_configure 'max degree of parallelism', 8;"
        )
        .is_empty());
    }
    #[test]
    fn cost_threshold_five_fires() {
        assert!(fires(
            sp_configure_known_bad,
            "EXEC sp_configure 'cost threshold for parallelism', 5;",
            "config.cost_threshold_default"
        ));
    }
    #[test]
    fn cost_threshold_fifty_does_not_fire() {
        assert!(run(
            sp_configure_known_bad,
            "EXEC sp_configure 'cost threshold for parallelism', 50;"
        )
        .is_empty());
    }
    #[test]
    fn sp_configure_unrelated_setting_does_not_fire() {
        assert!(run(
            sp_configure_known_bad,
            "EXEC sp_configure 'show advanced options', 1;"
        )
        .is_empty());
    }

    // ---- (f) RECOVERY SIMPLE ------------------------------------------------
    #[test]
    fn recovery_simple_fires_info() {
        let f = run(recovery_simple, "ALTER DATABASE Sales SET RECOVERY SIMPLE;");
        assert!(f.iter().any(|x| x.rule.0 == "config.recovery_model_simple"
            && x.severity == Severity::Info
            && x.location.is_some()));
    }
    #[test]
    fn recovery_full_does_not_fire() {
        assert!(run(recovery_simple, "ALTER DATABASE Sales SET RECOVERY FULL;").is_empty());
    }

    // ---- (g) DBCC TRACEON global --------------------------------------------
    #[test]
    fn traceon_global_fires() {
        assert!(fires(
            dbcc_traceon_global,
            "DBCC TRACEON (3226, -1);",
            "config.dbcc_traceon_global"
        ));
    }
    #[test]
    fn traceon_session_scoped_does_not_fire() {
        // No -1 → session-scoped, not a server-wide change.
        assert!(run(dbcc_traceon_global, "DBCC TRACEON (3226);").is_empty());
    }
    #[test]
    fn traceon_skips_lock_escalation_flags() {
        // 1211 / 1224 belong to the locking pack; this rule must stay silent.
        assert!(run(dbcc_traceon_global, "DBCC TRACEON (1211, -1);").is_empty());
        assert!(run(dbcc_traceon_global, "DBCC TRACEON (1224, -1);").is_empty());
    }
}
