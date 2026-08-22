// Statistics / cardinality settings rules.

use super::{finding, is_word, make_loc, RuleCtx};
use super::index_design::{all_sources_non_indexable, batch_ids, cte_name_set, statement_start};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

/// Locate ALTER DATABASE … SET … <option> <value-word>.
/// Returns the index of the option token and the value (next non-comma/non-paren word).
fn scan_alter_db_set_option<'a>(
    tokens: &'a [crate::tokens::Token<'a>],
    option_kw: &str,
) -> Vec<(usize, &'a crate::tokens::Token<'a>, &'a crate::tokens::Token<'a>)> {
    let mut hits = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        // Find ALTER DATABASE …
        if !is_word(&tokens[i], "ALTER") { i += 1; continue; }
        let mut j = i + 1;
        // Skip comments
        while j < tokens.len() && tokens[j].kind == TokKind::Comment { j += 1; }
        if j >= tokens.len() || !is_word(&tokens[j], "DATABASE") { i += 1; continue; }
        // Scan forward looking for SET, then the option token, then its value.
        // Stop at statement terminator `;` or end / next ALTER.
        let mut k = j + 1;
        let mut saw_set = false;
        while k < tokens.len() {
            let t = &tokens[k];
            if t.kind == TokKind::Comment { k += 1; continue; }
            if t.text == ";" { break; }
            if !saw_set {
                if is_word(t, "SET") { saw_set = true; }
                k += 1;
                continue;
            }
            if is_word(t, option_kw) {
                // Find the next non-comma/non-paren Word token as the value.
                let mut v = k + 1;
                while v < tokens.len() {
                    let vt = &tokens[v];
                    if vt.kind == TokKind::Comment {
                        v += 1;
                        continue;
                    }
                    if vt.text == "," || vt.text == "(" || vt.text == ")" || vt.text == "=" {
                        v += 1;
                        continue;
                    }
                    if vt.kind == TokKind::Word {
                        hits.push((k, t, vt));
                        break;
                    }
                    // any other token (number, string, semicolon) — stop searching for this option
                    break;
                }
            }
            k += 1;
        }
        i = j + 1;
    }
    hits
}

pub fn auto_create_stats_off(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (_, opt_tok, val_tok) in scan_alter_db_set_option(ctx.tokens, "AUTO_CREATE_STATISTICS") {
        if is_word(val_tok, "OFF") {
            out.push(finding(
                "stats.auto_create_stats_off",
                Severity::Error,
                "AUTO_CREATE_STATISTICS is OFF — the optimizer will use density guesses and cardinality estimates will be poor.",
                Some(make_loc(opt_tok)),
                Some("Without auto-create, the optimizer falls back to defaults (1 row for table variables, density guesses elsewhere). `ALTER DATABASE [x] SET AUTO_CREATE_STATISTICS ON;`.".into()),
            ));
        }
    }
    out
}

pub fn auto_update_stats_off(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (_, opt_tok, val_tok) in scan_alter_db_set_option(ctx.tokens, "AUTO_UPDATE_STATISTICS") {
        if is_word(val_tok, "OFF") {
            out.push(finding(
                "stats.auto_update_stats_off",
                Severity::Error,
                "AUTO_UPDATE_STATISTICS is OFF — histograms go stale and plans built on outdated stats misestimate cardinality.",
                Some(make_loc(opt_tok)),
                Some("Statistics go stale; plans built on outdated histograms underestimate cardinality. Turn it on; on OLTP also `AUTO_UPDATE_STATISTICS_ASYNC = ON`. Compat 130+ uses a dynamic threshold.".into()),
            ));
        }
    }
    out
}

pub fn update_stats_fullscan_lacking_incremental(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "UPDATE") { i += 1; continue; }
        // next non-comment must be STATISTICS
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokKind::Comment { j += 1; }
        if j >= tokens.len() || !is_word(&tokens[j], "STATISTICS") { i += 1; continue; }
        // Scan to end of statement (;) or end of tokens; check for WITH ... FULLSCAN and INCREMENTAL.
        let mut k = j + 1;
        let mut saw_with = false;
        let mut fullscan_tok: Option<usize> = None;
        let mut saw_incremental = false;
        while k < tokens.len() {
            let t = &tokens[k];
            if t.text == ";" { break; }
            if t.kind == TokKind::Comment { k += 1; continue; }
            if is_word(t, "WITH") { saw_with = true; }
            if saw_with && is_word(t, "FULLSCAN") && fullscan_tok.is_none() { fullscan_tok = Some(k); }
            if is_word(t, "INCREMENTAL") { saw_incremental = true; }
            k += 1;
        }
        if let Some(fk) = fullscan_tok {
            if !saw_incremental {
                out.push(finding(
                    "stats.update_statistics_fullscan_on_huge_table",
                    Severity::Info,
                    "UPDATE STATISTICS WITH FULLSCAN scans every page — on a multi-TB table this is hours of IO and rarely needed.",
                    Some(make_loc(&tokens[fk])),
                    Some("FULLSCAN on a multi-TB table is hours of IO. Default sampling is usually enough. Use `WITH SAMPLE n PERCENT, PERSIST_SAMPLE_PERCENT = ON` (2016 SP1 CU4+); for partitioned tables prefer `INCREMENTAL = ON`.".into()),
                ));
            }
        }
        i = j + 1;
    }
    out
}

/// Ascending-key hotspot: a trailing recent-window filter
///   `<col> >= DATEADD(<unit>, -N, <now-fn>())`
/// is the classic ascending-key problem — the statistics histogram's top step
/// lags real inserts, so the optimizer under-estimates the newest range and may
/// pick a nested-loops plan over a scan/seek. Anchored on DATEADD and walks back
/// over the comparison operator (which the tokenizer splits, e.g. `>=` → `>` `=`).
pub fn ascending_key_hotspot(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let ctes = cte_name_set(tokens);
    let batches = batch_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "DATEADD") { continue; }

        // Walk backward over the comparison operator run (`>`, `>=` → `>` `=`).
        let mut p = i.wrapping_sub(1);
        let mut saw_gt = false;
        let mut steps = 0u8;
        while let Some(tok) = tokens.get(p) {
            if tok.kind == TokKind::Comment { p = p.wrapping_sub(1); continue; }
            if tok.kind == TokKind::Punct && matches!(tok.text, ">" | "=" | "<") {
                if tok.text == ">" { saw_gt = true; }
                p = p.wrapping_sub(1);
                steps += 1;
                if steps > 2 { break; }
            } else {
                break;
            }
        }
        // Must be a `>`/`>=` lower-bound (trailing window), with a column on the left.
        if !saw_gt || steps == 0 { continue; }
        let Some(col) = tokens.get(p) else { continue };
        if col.kind != TokKind::Word || col.text.starts_with('@') { continue; }
        // The ascending-key story is about a persistent table's statistics
        // histogram lagging its inserts. A DMV, catalog view, TVF result
        // (`sys.fn_trace_gettable(...)`), CTE or a #temp table populated moments
        // ago by the same proc has no such histogram.
        let stmt_start = statement_start(tokens, i);
        if all_sources_non_indexable(tokens, stmt_start, i, &ctes, batches[i]) { continue; }

        // DATEADD( … ) must contain a negative offset (the `-N`) and a now() fn.
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokKind::Comment { j += 1; }
        if j >= tokens.len() || tokens[j].text != "(" { continue; }
        let mut depth = 0i32;
        let mut m = j;
        let (mut saw_minus, mut saw_now) = (false, false);
        while m < tokens.len() {
            let tk = &tokens[m];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; if depth == 0 { break; } }
            else if tk.text == "-" { saw_minus = true; }
            else if tk.kind == TokKind::Word {
                let lo = tk.text.to_ascii_lowercase();
                if matches!(lo.as_str(),
                    "getdate" | "getutcdate" | "sysdatetime" | "sysutcdatetime" | "current_timestamp")
                { saw_now = true; }
            }
            m += 1;
        }
        if saw_minus && saw_now {
            out.push(finding(
                "stats.ascending_key_hotspot",
                Severity::Info,
                format!("Trailing recent-window filter on `{}` vs now() is the classic ascending-key hotspot: the statistics histogram's top step lags real inserts, so the optimizer under-estimates the newest range and can pick a nested-loops plan over a scan/seek.", col.text),
                Some(make_loc(col)),
                Some("Keep statistics current on the ascending key (more frequent UPDATE STATISTICS, or trace flag 2371 pre-2016), or test OPTION (RECOMPILE) / a date-bucketed filtered index for the hot trailing range. Validate with actual-vs-estimated rows on the rightmost histogram step.".into()),
            ));
        }
    }
    out
}
