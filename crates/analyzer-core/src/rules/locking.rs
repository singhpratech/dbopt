use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

/// Rule 1: SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED
/// Detects a session-wide dirty-read isolation switch.
pub fn session_read_uncommitted(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // We scan for the strict contiguous (whitespace already stripped) word sequence.
    let seq = ["SET", "TRANSACTION", "ISOLATION", "LEVEL", "READ", "UNCOMMITTED"];
    if tokens.len() < seq.len() {
        return out;
    }
    for i in 0..=tokens.len() - seq.len() {
        let mut ok = true;
        for (k, kw) in seq.iter().enumerate() {
            if !is_word(&tokens[i + k], kw) {
                ok = false;
                break;
            }
        }
        if ok {
            out.push(finding(
                "locking.set_transaction_isolation_read_uncommitted",
                Severity::Error,
                "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED makes the entire session perform dirty reads.",
                Some(make_loc(&tokens[i])),
                Some("Session-wide dirty reads. Remove this. If you need non-blocking reads, enable `READ_COMMITTED_SNAPSHOT` at DB level; on 2025 evaluate `OPTIMIZED_LOCKING`.".into()),
            ));
        }
    }
    out
}

/// Rule 2: UPDATE / DELETE that has a WHERE clause but no TOP (n) batching.
/// Distinct from hygiene.unbounded_dml (which flags *missing* WHERE entirely).
pub fn unbounded_dml_lock_escalation(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        let is_update = is_word(t, "UPDATE");
        let is_delete = is_word(t, "DELETE");
        if !(is_update || is_delete) {
            continue;
        }

        // Look at the next non-comment token. For DELETE, allow an optional FROM.
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokKind::Comment {
            j += 1;
        }
        // If the very next token is TOP, the DML is already batched — skip.
        if j < tokens.len() && is_word(&tokens[j], "TOP") {
            continue;
        }
        // DELETE FROM <ident> — consume optional FROM
        if is_delete && j < tokens.len() && is_word(&tokens[j], "FROM") {
            j += 1;
            while j < tokens.len() && tokens[j].kind == TokKind::Comment {
                j += 1;
            }
        }
        // Require an identifier (Word) as the target table.
        if j >= tokens.len() || tokens[j].kind != TokKind::Word {
            continue;
        }
        // Filter out non-DML usages (e.g. "FOR UPDATE", "UPDATE STATISTICS", "DELETE FROM <temp>"
        // — we still flag temp-table DML with WHERE; just exclude `UPDATE STATISTICS`).
        if is_update && is_word(&tokens[j], "STATISTICS") {
            continue;
        }

        // Check for `WITH (... TABLOCK ...)` within the next 10 tokens of the table identifier
        // — intentional escalation, suppress.
        let look_end = (j + 10).min(tokens.len());
        let mut has_tablock_hint = false;
        let mut k = j + 1;
        while k < look_end {
            if is_word(&tokens[k], "WITH")
                && k + 1 < tokens.len()
                && tokens[k + 1].text == "("
            {
                // Scan up to a matching ')' (or look_end) for TABLOCK / TABLOCKX
                let mut m = k + 2;
                let mut depth = 1i32;
                while m < tokens.len() && depth > 0 {
                    if tokens[m].text == "(" {
                        depth += 1;
                    } else if tokens[m].text == ")" {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    } else if is_word(&tokens[m], "TABLOCK") || is_word(&tokens[m], "TABLOCKX") {
                        has_tablock_hint = true;
                    }
                    m += 1;
                }
                break;
            }
            k += 1;
        }
        if has_tablock_hint {
            continue;
        }

        // Walk forward to statement terminator looking for: WHERE (at top depth) and TOP (
        let mut has_where = false;
        let mut has_top_paren = false;
        let mut depth = 0i32;
        let mut p = j + 1;
        while p < tokens.len() {
            let tk = &tokens[p];
            if tk.text == "(" {
                depth += 1;
            } else if tk.text == ")" {
                depth -= 1;
                if depth < 0 {
                    break;
                }
            } else if depth == 0 && tk.text == ";" {
                break;
            } else if depth == 0 && is_word(tk, "WHERE") {
                has_where = true;
            } else if depth == 0 && is_word(tk, "TOP") {
                if let Some(n) = tokens.get(p + 1) {
                    if n.text == "(" {
                        has_top_paren = true;
                    }
                }
            }
            p += 1;
        }

        if has_where && !has_top_paren {
            out.push(finding(
                "locking.update_without_index",
                Severity::Warning,
                format!("{} … WHERE without TOP (n) batching risks lock escalation on large affected rowsets.", t.text.to_uppercase()),
                Some(make_loc(t)),
                Some("Bulk DML accumulates row/page locks and escalates at the 5,000-lock threshold, blocking the whole table. Batch into 1,000-5,000-row chunks: `DELETE TOP (1000) FROM ... WHERE ... ;` in a loop. On 2025 enable `OPTIMIZED_LOCKING` + ADR for LAQ.".into()),
            ));
        }
    }
    out
}

/// Rule 3: DBCC TRACEON (1211 / 1224) or -T1211 / -T1224 startup flags.
pub fn trace_flag_lock_escalation_disabled(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let rec: String = "Trace flag 1211 disables all lock escalation; 1224 disables it by lock count. Either causes error 1204 (out of lock space) under load. Use `ALTER TABLE ... SET (LOCK_ESCALATION = DISABLE)` per offending table instead.".into();

    for (i, t) in tokens.iter().enumerate() {
        // DBCC TRACEON ( 1211 | 1224 ...
        if is_word(t, "DBCC")
            && i + 3 < tokens.len()
            && is_word(&tokens[i + 1], "TRACEON")
            && tokens[i + 2].text == "("
        {
            // Scan numeric args until ')' for 1211 / 1224
            let mut j = i + 3;
            let mut depth = 1i32;
            while j < tokens.len() && depth > 0 {
                if tokens[j].text == "(" {
                    depth += 1;
                } else if tokens[j].text == ")" {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                } else if tokens[j].kind == TokKind::Number
                    && (tokens[j].text == "1211" || tokens[j].text == "1224")
                {
                    out.push(finding(
                        "locking.lock_escalation_disabled_globally",
                        Severity::Error,
                        format!("DBCC TRACEON ({}) disables lock escalation server-wide.", tokens[j].text),
                        Some(make_loc(&tokens[j])),
                        Some(rec.clone()),
                    ));
                }
                j += 1;
            }
            continue;
        }

        // -T1211 / -T1224 — may appear as a single Word "T1211" preceded by '-',
        // or as the bare numeric pattern '-' 'T' '1211' depending on tokenization.
        if t.text == "-" {
            if let Some(nxt) = tokens.get(i + 1) {
                let s = nxt.text;
                if matches!(s, "T1211" | "T1224" | "t1211" | "t1224") {
                    out.push(finding(
                        "locking.lock_escalation_disabled_globally",
                        Severity::Error,
                        format!("Startup flag {} disables lock escalation.", s),
                        Some(make_loc(nxt)),
                        Some(rec.clone()),
                    ));
                } else if (is_word(nxt, "T") || nxt.text == "T" || nxt.text == "t")
                    && i + 2 < tokens.len()
                    && tokens[i + 2].kind == TokKind::Number
                    && (tokens[i + 2].text == "1211" || tokens[i + 2].text == "1224")
                {
                    out.push(finding(
                        "locking.lock_escalation_disabled_globally",
                        Severity::Error,
                        format!("Startup flag -T{} disables lock escalation.", tokens[i + 2].text),
                        Some(make_loc(&tokens[i + 2])),
                        Some(rec.clone()),
                    ));
                }
            }
        }
    }
    out
}

/// Rule 4: ALTER DATABASE ... SET OPTIMIZED_LOCKING = ON without ACCELERATED_DATABASE_RECOVERY = ON in the same file.
/// 2025+ only.
pub fn optimized_locking_needs_adr(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2025 {
        return out;
    }
    let tokens = ctx.tokens;

    // First pass: check whether ACCELERATED_DATABASE_RECOVERY = ON appears anywhere in the file.
    let mut has_adr_on = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "ACCELERATED_DATABASE_RECOVERY") {
            // Look for `=` then `ON` within a small window (allow comments).
            let mut j = i + 1;
            // skip comments
            while j < tokens.len() && tokens[j].kind == TokKind::Comment {
                j += 1;
            }
            if j < tokens.len() && tokens[j].text == "=" {
                let mut k = j + 1;
                while k < tokens.len() && tokens[k].kind == TokKind::Comment {
                    k += 1;
                }
                if k < tokens.len() && is_word(&tokens[k], "ON") {
                    has_adr_on = true;
                    break;
                }
            }
        }
    }

    if has_adr_on {
        return out;
    }

    // Second pass: find OPTIMIZED_LOCKING = ON inside an ALTER DATABASE block.
    // We treat the block as the span from `ALTER DATABASE` until the next statement terminator `;` at depth 0.
    let mut i = 0usize;
    while i < tokens.len() {
        if is_word(&tokens[i], "ALTER")
            && i + 1 < tokens.len()
            && is_word(&tokens[i + 1], "DATABASE")
        {
            // Walk until ';' at depth 0.
            let mut j = i + 2;
            let mut depth = 0i32;
            while j < tokens.len() {
                let tk = &tokens[j];
                if tk.text == "(" {
                    depth += 1;
                } else if tk.text == ")" {
                    depth -= 1;
                } else if depth == 0 && tk.text == ";" {
                    break;
                } else if is_word(tk, "OPTIMIZED_LOCKING") {
                    // Check `= ON`
                    let mut k = j + 1;
                    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
                        k += 1;
                    }
                    if k < tokens.len() && tokens[k].text == "=" {
                        let mut m = k + 1;
                        while m < tokens.len() && tokens[m].kind == TokKind::Comment {
                            m += 1;
                        }
                        if m < tokens.len() && is_word(&tokens[m], "ON") {
                            out.push(finding(
                                "maintenance.adr_required_for_optimized_locking",
                                Severity::Info,
                                "OPTIMIZED_LOCKING = ON without ACCELERATED_DATABASE_RECOVERY = ON in the same script.",
                                Some(make_loc(tk)),
                                Some("OPTIMIZED_LOCKING (2025+) requires Accelerated Database Recovery to deliver its full benefit. Enable both: `ALTER DATABASE [x] SET ACCELERATED_DATABASE_RECOVERY = ON;` then `... SET OPTIMIZED_LOCKING = ON;`.".into()),
                            ));
                        }
                    }
                }
                j += 1;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}
