// Transaction & error-handling smells.
//
// These rules reason about explicit-transaction structure at the *batch* level:
// where a BEGIN TRAN is, whether a COMMIT/ROLLBACK exists (possibly inside a
// CATCH block), whether the batch is wrapped in TRY/CATCH, whether XACT_ABORT is
// set, and whether DDL is being run inside a long-held transaction.
//
// False positives are the worst outcome, so every rule reads the WHOLE token
// stream before firing and guards against the legitimate idioms (COMMIT inside
// CATCH, ROLLBACK in an error path, single-statement implicit transactions, the
// `transaction` keyword appearing only as an identifier, etc.).
//
// IMPORTANT — bracketed/quoted identifiers: the lexer emits `[COMMIT]` or
// `"delete"` as a SINGLE Word token *including* the delimiters, and the shared
// `is_word` helper strips `[]` before comparing, so `is_word("[COMMIT]","COMMIT")`
// is TRUE. That means a column legitimately named [COMMIT], [Rollback], [delete]
// or [insert] (common in audit/git/workflow/state-machine schemas) would match a
// keyword. Transaction keywords are control-flow verbs, never sensibly used as a
// bare keyword when delimited, so these rules use `is_kw` (bare-keyword only)
// instead of `is_word`, which refuses any token that is a delimited identifier.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{word_eq_ci, Token, TokKind};

/// True if `t` is a *bare* keyword equal (case-insensitively) to `kw`.
///
/// Unlike `is_word`, this returns `false` for *bracketed* identifiers — a
/// `[bracketed]` token (which the lexer emits whole, brackets included) can
/// never be a T-SQL keyword, it is always an identifier. This is the guard that
/// stops `[COMMIT]`, `[Rollback]`, `[delete]`, `[insert]` (etc.) used as
/// column/object names from being read as the COMMIT/ROLLBACK/DELETE/INSERT
/// keywords.
///
/// NOTE: double-quoted identifiers (`"commit"`) are NOT a single token — the
/// lexer emits `"`, `commit`, `"` separately — so this function alone cannot
/// see them. Callers that scan the token stream additionally use
/// `is_quoted_ident` to reject a word sandwiched between double quotes.
fn is_kw(t: &Token<'_>, kw: &str) -> bool {
    if !matches!(t.kind, TokKind::Word) {
        return false;
    }
    let s = t.text;
    // Bracketed identifiers are never keywords.
    if s.starts_with('[') || s.starts_with('"') {
        return false;
    }
    word_eq_ci(s, kw)
}

/// True if the Word token at index `i` is really a double-quoted identifier
/// (`"commit"`), i.e. it is immediately wrapped by `"` Punct tokens. The lexer
/// has no double-quote-string branch, so `"commit"` arrives as the three tokens
/// `"` `commit` `"`; under QUOTED_IDENTIFIER ON (the default) that inner word is
/// an identifier, never a keyword.
fn is_quoted_ident(tokens: &[Token<'_>], i: usize) -> bool {
    let prev_quote = i > 0 && tokens[i - 1].text == "\"";
    let next_quote = i + 1 < tokens.len() && tokens[i + 1].text == "\"";
    prev_quote && next_quote
}

/// Index of the next non-comment, non-whitespace token at or after `from`.
/// (The lexer already drops whitespace, so this only skips comments.)
fn next_code(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
        k += 1;
    }
    k
}

/// True if the token at index `i` opens an explicit transaction:
/// `BEGIN TRAN` / `BEGIN TRANSACTION` (optionally with a name) and NOT
/// `BEGIN TRY` / a bare procedural `BEGIN` block / `BEGIN DISTRIBUTED TRAN…`.
/// Returns the index of the TRAN/TRANSACTION keyword on a match.
fn begin_tran_at(tokens: &[Token<'_>], i: usize) -> Option<usize> {
    if !is_kw(&tokens[i], "BEGIN") {
        return None;
    }
    let mut j = next_code(tokens, i + 1);
    // BEGIN DISTRIBUTED TRAN… — still an explicit transaction.
    if j < tokens.len() && is_kw(&tokens[j], "DISTRIBUTED") {
        j = next_code(tokens, j + 1);
    }
    if j < tokens.len() && (is_kw(&tokens[j], "TRAN") || is_kw(&tokens[j], "TRANSACTION")) {
        return Some(j);
    }
    None
}

/// True if the token at index `i` is a `SAVE TRAN`/`SAVE TRANSACTION` savepoint
/// declaration. Returns the index of the savepoint name token if one follows.
fn save_tran_name_at(tokens: &[Token<'_>], i: usize) -> Option<usize> {
    if !is_kw(&tokens[i], "SAVE") {
        return None;
    }
    let j = next_code(tokens, i + 1);
    if j >= tokens.len() || !(is_kw(&tokens[j], "TRAN") || is_kw(&tokens[j], "TRANSACTION")) {
        return None;
    }
    let k = next_code(tokens, j + 1);
    // SAVE TRAN <name> — name may be an identifier or a @variable.
    if k < tokens.len() && tokens[k].kind == TokKind::Word {
        Some(k)
    } else {
        None
    }
}

/// True if index `i` is the start of a COMMIT (`COMMIT` / `COMMIT TRAN…` /
/// `COMMIT WORK`). A bare `COMMIT` is valid T-SQL, so we accept it on its own.
/// Refuses a `[COMMIT]` or `"commit"` delimited/quoted identifier.
fn is_commit_at(tokens: &[Token<'_>], i: usize) -> bool {
    is_kw(&tokens[i], "COMMIT") && !is_quoted_ident(tokens, i)
}

/// True if index `i` is the start of a ROLLBACK (`ROLLBACK` / `ROLLBACK TRAN…`
/// / `ROLLBACK WORK`). Refuses a `[Rollback]` or `"rollback"` identifier.
fn is_rollback_at(tokens: &[Token<'_>], i: usize) -> bool {
    is_kw(&tokens[i], "ROLLBACK") && !is_quoted_ident(tokens, i)
}

/// True if index `i` is a ROLLBACK that targets a *savepoint* whose name appears
/// in `savepoints` — i.e. `ROLLBACK [TRAN|TRANSACTION] <name>` where `<name>`
/// was previously declared with `SAVE TRAN <name>`. A savepoint rollback is the
/// documented inner-scope idiom (it rolls back only this proc's work, not the
/// caller's whole transaction) and must NOT be treated as a top-level close.
fn rolls_back_to_savepoint(tokens: &[Token<'_>], i: usize, savepoints: &[String]) -> bool {
    if !is_kw(&tokens[i], "ROLLBACK") {
        return false;
    }
    let mut j = next_code(tokens, i + 1);
    if j < tokens.len() && (is_kw(&tokens[j], "TRAN") || is_kw(&tokens[j], "TRANSACTION")) {
        j = next_code(tokens, j + 1);
    }
    // The next token must be a savepoint name we have seen declared.
    if j < tokens.len() && tokens[j].kind == TokKind::Word {
        let name = tokens[j].text;
        return savepoints.iter().any(|s| word_eq_ci(s, name));
    }
    false
}

/// Collect every savepoint name declared by `SAVE TRAN <name>` in the batch.
fn collect_savepoints(tokens: &[Token<'_>]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        if let Some(k) = save_tran_name_at(tokens, i) {
            out.push(tokens[k].text.to_string());
        }
    }
    out
}

/// True if the COMMIT/ROLLBACK at index `i` is guarded by an `@@TRANCOUNT`
/// (or `XACT_STATE()`) check that runs into the same statement — the documented
/// nested/participating-proc idiom `IF @@TRANCOUNT > 0 COMMIT;`. We look back a
/// short window for the guard token, stopping at a statement terminator.
///
/// NOTE: the lexer splits `@@TRANCOUNT` into the two Word tokens `@` and
/// `@TRANCOUNT` (the word-continuation set excludes a second `@`), so we match a
/// word whose `@`-stripped text is `TRANCOUNT`. `XACT_STATE` is a function call,
/// so it arrives as the plain word `XACT_STATE`.
fn guarded_by_trancount(tokens: &[Token<'_>], i: usize) -> bool {
    let mut j = i;
    let mut steps = 0;
    while j > 0 && steps < 24 {
        j -= 1;
        steps += 1;
        let tk = &tokens[j];
        if tk.text == ";" {
            return false;
        }
        if tk.kind == TokKind::Word {
            let bare = tk.text.trim_start_matches('@');
            if word_eq_ci(bare, "TRANCOUNT") || word_eq_ci(bare, "XACT_STATE") {
                return true;
            }
        }
    }
    false
}

/// Whether `BEGIN TRY` appears anywhere in the token stream.
fn has_try_block(tokens: &[Token<'_>]) -> bool {
    for i in 0..tokens.len() {
        if is_kw(&tokens[i], "BEGIN") {
            let j = next_code(tokens, i + 1);
            if j < tokens.len() && is_kw(&tokens[j], "TRY") {
                return true;
            }
        }
    }
    false
}

/// Whether `SET XACT_ABORT ON` appears anywhere in the token stream. Tolerates
/// comments between the words AND the idiomatic comma-combined multi-option SET
/// form `SET XACT_ABORT, NOCOUNT ON;` (one shared trailing ON turns every listed
/// option ON). We deliberately do NOT accept `SET XACT_ABORT OFF`.
fn has_xact_abort_on(tokens: &[Token<'_>]) -> bool {
    for i in 0..tokens.len() {
        if !is_kw(&tokens[i], "SET") {
            continue;
        }
        // Find XACT_ABORT anywhere in this SET's option list, not just first:
        // `SET NOCOUNT, XACT_ABORT ON` is the same statement written the other
        // way round, and only the XACT_ABORT-first spelling was recognised.
        let mut j = next_code(tokens, i + 1);
        while j < tokens.len()
            && !is_kw(&tokens[j], "XACT_ABORT")
            && (tokens[j].text == "," || tokens[j].kind == TokKind::Word)
            && !is_kw(&tokens[j], "ON")
            && !is_kw(&tokens[j], "OFF")
            && tokens[j].text != ";"
        {
            j = next_code(tokens, j + 1);
        }
        if j >= tokens.len() || !is_kw(&tokens[j], "XACT_ABORT") {
            continue;
        }
        let k = next_code(tokens, j + 1);
        if k >= tokens.len() {
            continue;
        }
        // Direct form: SET XACT_ABORT ON
        if is_kw(&tokens[k], "ON") {
            return true;
        }
        // Comma-combined form: SET XACT_ABORT, NOCOUNT[, ...] ON;
        // After XACT_ABORT comes ',' then a list of `<option>[, <option>]*`
        // closed by a single shared ON. Walk the option list; if it is purely
        // `, <word>` groups terminated by ON (before any `;`/OFF), XACT_ABORT is
        // turned ON.
        if tokens[k].text == "," {
            let mut m = k;
            let mut ok = false;
            while m < tokens.len() {
                let t = &tokens[m];
                if t.kind == TokKind::Comment {
                    m += 1;
                    continue;
                }
                if t.text == "," {
                    m += 1;
                    continue;
                }
                if t.text == ";" {
                    break;
                }
                if matches!(t.kind, TokKind::Word) {
                    if is_kw(t, "ON") {
                        ok = true;
                        break;
                    }
                    if is_kw(t, "OFF") {
                        // Shared OFF — XACT_ABORT is being turned off.
                        break;
                    }
                    // Another option name in the list (NOCOUNT, ANSI_NULLS, …).
                    m += 1;
                    continue;
                }
                // Anything else (=, number, punct) means this is not a simple
                // multi-option SET list; give up on this candidate.
                break;
            }
            if ok {
                return true;
            }
        }
    }
    false
}

/// True if the DML verb at index `i` is really a `UPDATE STATISTICS` maintenance
/// command rather than row DML. `UPDATE STATISTICS <table>` is a DDL-ish
/// maintenance op that does not leave a transaction half-applied, so it must not
/// be counted toward the multi-statement-DML threshold.
fn is_update_statistics(tokens: &[Token<'_>], i: usize) -> bool {
    if !is_kw(&tokens[i], "UPDATE") {
        return false;
    }
    let j = next_code(tokens, i + 1);
    j < tokens.len() && is_kw(&tokens[j], "STATISTICS")
}

/// A DML statement keyword that *starts a statement* inside a transaction body.
/// We count these to tell a multi-statement transaction (needs XACT_ABORT) from
/// a single-statement one.
///
/// Guards against three documented false-positive shapes:
///   * delimited identifiers — `[delete]`, `[insert]`, `"update"` used as column
///     or object names are NOT keywords (handled by `is_kw`);
///   * keywords used mid-expression / as a column name — we only count a verb at
///     statement-start position (preceded by `;`, `BEGIN`, `THEN`, batch start,
///     etc.), not one buried in a SET list or expression;
///   * `UPDATE STATISTICS` — maintenance, not row DML.
fn is_data_stmt_start(tokens: &[Token<'_>], i: usize) -> bool {
    let t = &tokens[i];
    let is_dml = is_kw(t, "INSERT")
        || is_kw(t, "UPDATE")
        || is_kw(t, "DELETE")
        || is_kw(t, "MERGE");
    if !is_dml {
        return false;
    }
    // UPDATE STATISTICS is not row DML.
    if is_update_statistics(tokens, i) {
        return false;
    }
    // Must be at statement-start position: scan back over comments to the
    // previous code token; it must be a separator/opener, not part of an ongoing
    // expression or assignment list (which is what `SET col1 = 1, col2 = 2`
    // looks like). This is belt-and-suspenders on top of `is_kw` so a keyword
    // appearing inside an expression cannot be recounted as a statement.
    let mut p = i;
    while p > 0 {
        p -= 1;
        if tokens[p].kind == TokKind::Comment {
            continue;
        }
        let prev = &tokens[p];
        // Statement separators / openers that legitimately precede a DML verb.
        let opener = prev.text == ";"
            || is_kw(prev, "BEGIN")
            || is_kw(prev, "THEN")
            || is_kw(prev, "ELSE")
            || is_kw(prev, "END")
            || is_kw(prev, "GO")
            || is_kw(prev, "AS");
        return opener;
    }
    // No preceding code token → batch start → it's a statement start.
    true
}

/// (a) BEGIN TRAN … COMMIT with NO TRY/CATCH anywhere in the batch and no
/// rollback path. Without TRY/CATCH a mid-transaction error leaves the
/// transaction open (or, with some errors, doomed) and the COMMIT may never run
/// — connection pooling then hands the open transaction to the next caller.
///
/// Conservative: fires once per BEGIN TRAN, only when the file has NO BEGIN TRY
/// at all AND no ROLLBACK at all. If either exists we assume the author has an
/// error path and stay silent.
pub fn begin_tran_without_try_catch(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    if has_try_block(tokens) {
        return out;
    }
    // If there is any ROLLBACK anywhere, the author has thought about the error
    // path even without TRY/CATCH (e.g. checking @@ERROR). Don't nag.
    let any_rollback = (0..tokens.len()).any(|i| is_rollback_at(tokens, i));
    if any_rollback {
        return out;
    }

    let mut i = 0;
    while i < tokens.len() {
        if let Some(_tran_kw) = begin_tran_at(tokens, i) {
            // Confirm a COMMIT exists later in the stream — otherwise this is the
            // "no matching commit" smell handled by (c), not this rule.
            let has_commit = (i + 1..tokens.len()).any(|k| is_commit_at(tokens, k));
            if has_commit {
                out.push(finding(
                    "tran.begin_without_try_catch",
                    Severity::Warning,
                    "Explicit BEGIN TRAN … COMMIT with no TRY/CATCH and no rollback path. A mid-transaction error leaves the transaction open (the COMMIT never runs) and pooled connections can inherit it.",
                    Some(make_loc(&tokens[i])),
                    Some("Wrap the work and add a rollback path:\nBEGIN TRAN\n  INSERT ...;\n  UPDATE ...;\nCOMMIT;\n  -->\nSET XACT_ABORT ON;\nBEGIN TRY\n    BEGIN TRAN;\n      INSERT ...;\n      UPDATE ...;\n    COMMIT;\nEND TRY\nBEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\nEND CATCH;".into()),
                ));
            }
            i = _tran_kw + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// (c) BEGIN TRAN with NO matching COMMIT or ROLLBACK anywhere in the batch.
/// This leaves the transaction open at end-of-batch. Reads the whole stream
/// first (the COMMIT may legitimately live inside a CATCH block far below).
/// `ROLLBACK TRAN[SACTION] <savepoint_name>` — an inner unwind, not a close.
/// A bare `ROLLBACK`/`ROLLBACK TRANSACTION` (optionally followed by `;`) closes
/// the outermost transaction; a name after it does not.
fn is_savepoint_rollback(tokens: &[Token<'_>], i: usize) -> bool {
    let mut j = next_code(tokens, i + 1);
    if j < tokens.len() && (is_kw(&tokens[j], "TRAN") || is_kw(&tokens[j], "TRANSACTION")) {
        j = next_code(tokens, j + 1);
    }
    tokens
        .get(j)
        .map(|t| t.kind == TokKind::Word && !t.text.starts_with('@'))
        .unwrap_or(false)
}

pub fn begin_tran_without_commit(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Count opens vs. closes across the whole batch. We don't try to match them
    // structurally (nesting/savepoints make that fragile); we only fire when
    // there is at least one BEGIN TRAN and ZERO COMMIT and ZERO ROLLBACK — the
    // unambiguous "open transaction never closed" case.
    let mut first_begin: Option<usize> = None;
    let mut begin_count = 0usize;
    let mut close_count = 0usize;

    let mut i = 0;
    while i < tokens.len() {
        if let Some(tran_kw) = begin_tran_at(tokens, i) {
            if first_begin.is_none() {
                first_begin = Some(i);
            }
            begin_count += 1;
            i = tran_kw + 1;
            continue;
        }
        // `ROLLBACK TRANSACTION sp1` unwinds to a savepoint; the outer
        // transaction is still open afterwards, so counting it as a close hid
        // the exact case this rule exists for.
        if is_commit_at(tokens, i) || (is_rollback_at(tokens, i) && !is_savepoint_rollback(tokens, i)) {
            close_count += 1;
        }
        i += 1;
    }

    if begin_count > 0 && close_count == 0 {
        if let Some(b) = first_begin {
            out.push(finding(
                "tran.begin_without_commit",
                Severity::Warning,
                "BEGIN TRAN has no COMMIT or ROLLBACK anywhere in the batch — the transaction stays open past end-of-batch, holding locks and (on a pooled connection) leaking into the next request.",
                Some(make_loc(&tokens[b])),
                Some("Close every transaction you open. Add the matching COMMIT (and a ROLLBACK error path):\nBEGIN TRAN;\n  ...;\n  -->\nBEGIN TRY\n    BEGIN TRAN;\n      ...;\n    COMMIT;\nEND TRY\nBEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\nEND CATCH;".into()),
            ));
        }
    }
    out
}

/// (e) COMMIT or ROLLBACK with NO BEGIN TRAN anywhere in the batch — a classic
/// nested-transaction / @@TRANCOUNT misunderstanding (e.g. assuming an inner
/// proc owns the transaction it merely participates in). Committing/rolling back
/// a transaction you didn't open raises error 3902/3903 or unexpectedly affects
/// the caller's transaction.
///
/// Suppressed for the documented inner-scope idioms:
///   * `IF @@TRANCOUNT > 0 COMMIT;` / `IF @@TRANCOUNT > 0 ROLLBACK;` — the close
///     is explicitly guarded by a transaction-count check (a participating proc
///     finalizing only when it actually owns the outermost transaction);
///   * `SAVE TRAN sp; … ROLLBACK TRAN sp;` — rolling back to a *savepoint* the
///     batch declared is not a top-level close at all.
/// Also refuses delimited identifiers (`[COMMIT]`, `[Rollback]` as column names).
pub fn commit_rollback_without_begin(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let any_begin = (0..tokens.len()).any(|i| begin_tran_at(tokens, i).is_some());
    if any_begin {
        return out;
    }

    let savepoints = collect_savepoints(tokens);

    // Find the first *unguarded, top-level* COMMIT/ROLLBACK and anchor there.
    // SAVE TRAN by itself is not a close, so it won't trip this.
    let mut i = 0;
    while i < tokens.len() {
        let commit = is_commit_at(tokens, i);
        let rollback = is_rollback_at(tokens, i);
        if commit || rollback {
            // Savepoint rollback (`ROLLBACK TRAN sp`) is inner-scope, not a close.
            if rollback && rolls_back_to_savepoint(tokens, i, &savepoints) {
                i += 1;
                continue;
            }
            // @@TRANCOUNT / XACT_STATE() guarded close is the documented idiom.
            if guarded_by_trancount(tokens, i) {
                i += 1;
                continue;
            }
            let verb = if commit { "COMMIT" } else { "ROLLBACK" };
            out.push(finding(
                "tran.close_without_begin",
                Severity::Warning,
                format!("{verb} with no BEGIN TRAN in this batch. If a caller (or outer proc) owns the transaction, committing/rolling back here changes THEIR transaction; if nobody does, this raises error 3903/3902 ('no corresponding BEGIN TRANSACTION')."),
                Some(make_loc(&tokens[i])),
                Some("A procedure should only finalize a transaction it started. Guard on @@TRANCOUNT, or use a savepoint so an inner proc rolls back only its own work:\nROLLBACK;  -- assumes a transaction exists\n  -->\nIF @@TRANCOUNT > 0 AND XACT_STATE() = -1 ROLLBACK;   -- or, for inner scope:\nDECLARE @sp NVARCHAR(32) = 'proc_sp';\nIF @@TRANCOUNT > 0 SAVE TRAN @sp;  -- ... later: IF @@TRANCOUNT > 0 ROLLBACK TRAN @sp;".into()),
            ));
            // One finding per batch is enough; avoid spamming on every COMMIT.
            break;
        }
        i += 1;
    }
    out
}

/// (b) A multi-statement explicit transaction (≥2 DML statements between a
/// BEGIN TRAN and its COMMIT/ROLLBACK or end-of-batch) with NO `SET XACT_ABORT
/// ON` in the batch. Without XACT_ABORT, many run-time errors abort only the
/// current statement and leave the transaction open and partially applied.
pub fn dml_batch_missing_xact_abort(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    if has_xact_abort_on(tokens) {
        return out;
    }

    let mut i = 0;
    while i < tokens.len() {
        let Some(tran_kw) = begin_tran_at(tokens, i) else {
            i += 1;
            continue;
        };
        // Scan the transaction body: from after the BEGIN TRAN keyword until the
        // first COMMIT/ROLLBACK or end-of-stream. Count distinct DML *statements*
        // (statement-start verbs only — not keywords used as column names, and
        // not UPDATE STATISTICS).
        let mut dml = 0usize;
        let mut j = tran_kw + 1;
        while j < tokens.len() {
            if is_commit_at(tokens, j) || is_rollback_at(tokens, j) {
                break;
            }
            // A new BEGIN TRAN means nested scope; stop counting for this one.
            if begin_tran_at(tokens, j).is_some() {
                break;
            }
            if is_data_stmt_start(tokens, j) {
                dml += 1;
            }
            j += 1;
        }

        if dml >= 2 {
            out.push(finding(
                "tran.missing_xact_abort",
                Severity::Warning,
                format!("Multi-statement transaction ({dml} DML statements) with no `SET XACT_ABORT ON`. Without it, many run-time errors abort only the failing statement and leave the transaction open and half-applied."),
                Some(make_loc(&tokens[i])),
                Some("Set XACT_ABORT ON so any run-time error aborts and rolls back the whole transaction, and pair it with TRY/CATCH:\nBEGIN TRAN;\n  INSERT ...;\n  UPDATE ...;\nCOMMIT;\n  -->\nSET XACT_ABORT ON;\nBEGIN TRY\n    BEGIN TRAN;\n      INSERT ...;\n      UPDATE ...;\n    COMMIT;\nEND TRY\nBEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\nEND CATCH;".into()),
            ));
        }
        i = j; // continue past the body we just scanned
        if i <= tran_kw {
            i = tran_kw + 1;
        }
    }
    out
}

/// (d) DDL (CREATE / ALTER / DROP / TRUNCATE) inside an explicit transaction.
/// Schema changes inside a transaction hold a schema-modification (Sch-M) lock
/// for the whole transaction lifetime — if the transaction also waits on user
/// input or a long operation, every reader of that object blocks. We only fire
/// for DDL that lands strictly between a BEGIN TRAN and its COMMIT/ROLLBACK.
pub fn ddl_inside_explicit_tran(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let mut i = 0;
    while i < tokens.len() {
        let Some(tran_kw) = begin_tran_at(tokens, i) else {
            i += 1;
            continue;
        };
        // Walk the body to the first COMMIT/ROLLBACK (or a nested BEGIN TRAN /
        // end-of-stream) looking for a DDL verb that begins a statement.
        let mut j = tran_kw + 1;
        while j < tokens.len() {
            if is_commit_at(tokens, j) || is_rollback_at(tokens, j) {
                break;
            }
            if begin_tran_at(tokens, j).is_some() {
                break;
            }
            // CREATE / ALTER / DROP — but exclude the procedural ALTER that is
            // really part of a `SET`/index op we don't care about, and exclude
            // `CREATE TABLE #temp` (creating a temp object inside a tran is the
            // legitimate idiom and does not hold a user-object Sch-M lock).
            let verb = if is_kw(&tokens[j], "CREATE") {
                Some("CREATE")
            } else if is_kw(&tokens[j], "ALTER") {
                Some("ALTER")
            } else if is_kw(&tokens[j], "DROP") {
                Some("DROP")
            } else if is_kw(&tokens[j], "TRUNCATE") {
                Some("TRUNCATE")
            } else {
                None
            };

            if let Some(v) = verb {
                // Look at the object kind keyword that follows.
                let k = next_code(tokens, j + 1);
                let obj = tokens.get(k);
                // ALTER TABLE ... is the most dangerous (Sch-M for the table). But
                // we suppress when the very next identifier names a temp/table
                // variable, since those are session-scoped and not shared.
                let names_temp = {
                    // Find the first identifier after the object-kind keyword.
                    let m = next_code(tokens, k + 1);
                    tokens
                        .get(m)
                        .map(|t| t.kind == TokKind::Word && (t.text.starts_with('#') || t.text.starts_with('@')))
                        .unwrap_or(false)
                };

                let object_word = obj.map(|t| t.kind == TokKind::Word).unwrap_or(false);
                // Only flag DDL against persistent schema objects (TABLE / INDEX /
                // VIEW / PROC etc.). Skip the temp/var case.
                let is_schema_object = obj
                    .map(|t| {
                        is_word(t, "TABLE")
                            || is_word(t, "INDEX")
                            || is_word(t, "VIEW")
                            || is_word(t, "PROCEDURE")
                            || is_word(t, "PROC")
                            || is_word(t, "FUNCTION")
                            || is_word(t, "TRIGGER")
                    })
                    .unwrap_or(false);

                if object_word && is_schema_object && !names_temp {
                    out.push(finding(
                        "tran.ddl_inside_explicit_tran",
                        Severity::Warning,
                        format!("{v} {} runs inside an explicit transaction. DDL takes a schema-modification (Sch-M) lock held for the transaction's entire lifetime — if the transaction is long-lived or waits on anything, every reader of that object blocks.", obj.unwrap().text),
                        Some(make_loc(&tokens[j])),
                        Some("Keep schema changes out of long-lived transactions, or make the transaction as short as possible and run DDL last. Never interleave DDL with steps that wait on user input or external calls:\nBEGIN TRAN;\n  ALTER TABLE dbo.Orders ADD Note nvarchar(200);\n  -- ... long-running work / waits ...\nCOMMIT;\n  -->\n-- run the ALTER on its own (auto-committed) statement, off the hot path\nALTER TABLE dbo.Orders ADD Note nvarchar(200);".into()),
                    ));
                }
                // Skip past this DDL statement's verb to avoid double-counting the
                // object keyword.
                j = k;
            }
            j += 1;
        }
        i = j;
        if i <= tran_kw {
            i = tran_kw + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::tokens::tokenize;
    use crate::rules::RuleCtx;
    use crate::Engine;
    use crate::findings::Finding;

    fn run(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str, ver: Option<u16>) -> Vec<Finding> {
        let tokens = tokenize(sql);
        let ctx = RuleCtx { src: sql, tokens: &tokens, server_version: ver, engine: Engine::SqlServer };
        f(&ctx)
    }

    fn fires(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str, ver: Option<u16>, id: &str) -> bool {
        run(f, sql, ver).iter().any(|x| x.rule.0 == id && x.location.is_some())
    }

    // ---- (a) begin_tran_without_try_catch -------------------------------

    #[test]
    fn try_catch_positive() {
        let sql = "BEGIN TRAN;\n  INSERT INTO dbo.A(x) VALUES (1);\n  UPDATE dbo.B SET y = 2;\nCOMMIT;";
        assert!(fires(super::begin_tran_without_try_catch, sql, Some(2022), "tran.begin_without_try_catch"));
    }

    #[test]
    fn try_catch_negative_has_try() {
        // Already wrapped in TRY/CATCH with rollback — must not fire.
        let sql = "SET XACT_ABORT ON;\nBEGIN TRY\n  BEGIN TRAN;\n    INSERT INTO dbo.A(x) VALUES (1);\n  COMMIT;\nEND TRY\nBEGIN CATCH\n  IF @@TRANCOUNT > 0 ROLLBACK;\n  THROW;\nEND CATCH;";
        assert!(!fires(super::begin_tran_without_try_catch, sql, Some(2022), "tran.begin_without_try_catch"));
    }

    #[test]
    fn try_catch_negative_no_tran() {
        // Plain auto-commit DML — no explicit transaction at all.
        let sql = "INSERT INTO dbo.A(x) VALUES (1);\nUPDATE dbo.B SET y = 2;";
        assert!(!fires(super::begin_tran_without_try_catch, sql, Some(2022), "tran.begin_without_try_catch"));
    }

    // ---- (c) begin_tran_without_commit ----------------------------------

    #[test]
    fn open_tran_positive() {
        let sql = "BEGIN TRANSACTION;\n  UPDATE dbo.Accounts SET bal = bal - 100 WHERE id = 1;";
        assert!(fires(super::begin_tran_without_commit, sql, Some(2022), "tran.begin_without_commit"));
    }

    #[test]
    fn open_tran_negative_committed() {
        // COMMIT lives inside the CATCH path far below — reading the whole batch
        // must clear the finding.
        let sql = "BEGIN TRY\n  BEGIN TRAN;\n    UPDATE dbo.Accounts SET bal = bal - 100;\n  COMMIT;\nEND TRY\nBEGIN CATCH\n  ROLLBACK;\nEND CATCH;";
        assert!(!fires(super::begin_tran_without_commit, sql, Some(2022), "tran.begin_without_commit"));
    }

    // ---- (e) commit_rollback_without_begin ------------------------------

    #[test]
    fn close_without_begin_positive() {
        let sql = "UPDATE dbo.A SET x = 1;\nROLLBACK;";
        assert!(fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    #[test]
    fn close_without_begin_negative_has_begin() {
        let sql = "BEGIN TRAN;\n  UPDATE dbo.A SET x = 1;\nCOMMIT;";
        assert!(!fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    // ---- FP regression tests (empirically confirmed false positives) ----

    /// FP: bracketed identifiers [COMMIT] / [Rollback] are column names, not the
    /// COMMIT/ROLLBACK keywords. There is no transaction syntax at all.
    #[test]
    fn close_without_begin_negative_bracketed_identifiers() {
        let sql = "SELECT [COMMIT], [Rollback] FROM dbo.GitCommitLog;";
        assert!(!fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    /// FP: quoted identifiers "commit" / "rollback" are likewise just names.
    #[test]
    fn close_without_begin_negative_quoted_identifiers() {
        let sql = "SELECT \"commit\", \"rollback\" FROM dbo.AuditLog;";
        assert!(!fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    /// FP: an @@TRANCOUNT-guarded COMMIT in a participating proc is the
    /// documented inner-scope idiom, not an orphan close.
    #[test]
    fn close_without_begin_negative_trancount_guarded_commit() {
        let sql = "CREATE PROCEDURE dbo.PostStep\nAS\nBEGIN\n  -- caller (or outer proc) owns the transaction; we only finalize our scope\n  IF @@TRANCOUNT > 0 COMMIT;\nEND";
        assert!(!fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    /// FP: rolling back to a savepoint the batch itself declared is inner-scope,
    /// not a top-level close. (SAVE TRAN sp; … ROLLBACK TRANSACTION sp;)
    #[test]
    fn close_without_begin_negative_savepoint_rollback() {
        let sql = "SAVE TRANSACTION sp;\n  UPDATE dbo.A SET x = 1;\nIF @@ERROR <> 0 ROLLBACK TRANSACTION sp;";
        assert!(!fires(super::commit_rollback_without_begin, sql, Some(2022), "tran.close_without_begin"));
    }

    // ---- (b) dml_batch_missing_xact_abort -------------------------------

    #[test]
    fn missing_xact_abort_positive() {
        let sql = "BEGIN TRAN;\n  INSERT INTO dbo.A(x) VALUES (1);\n  UPDATE dbo.B SET y = 2;\n  DELETE FROM dbo.C WHERE z = 3;\nCOMMIT;";
        assert!(fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    #[test]
    fn missing_xact_abort_negative_set() {
        let sql = "SET XACT_ABORT ON;\nBEGIN TRAN;\n  INSERT INTO dbo.A(x) VALUES (1);\n  UPDATE dbo.B SET y = 2;\nCOMMIT;";
        assert!(!fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    #[test]
    fn missing_xact_abort_negative_single_dml() {
        // Single-statement transaction does not need XACT_ABORT for atomicity.
        let sql = "BEGIN TRAN;\n  UPDATE dbo.B SET y = 2;\nCOMMIT;";
        assert!(!fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    /// FP: a single UPDATE whose SET list assigns bracketed columns [delete] /
    /// [insert] must NOT be miscounted as three DML statements.
    #[test]
    fn missing_xact_abort_negative_bracketed_columns() {
        let sql = "BEGIN TRAN;\n  UPDATE dbo.T SET [delete] = 1, [insert] = 2 WHERE id = 5;\nCOMMIT;";
        assert!(!fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    /// FP: two UPDATE STATISTICS maintenance commands are not row DML.
    #[test]
    fn missing_xact_abort_negative_update_statistics() {
        let sql = "BEGIN TRAN;\n  UPDATE STATISTICS dbo.BigTable;\n  UPDATE STATISTICS dbo.OtherTable;\nCOMMIT;";
        assert!(!fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    /// FP: the comma-combined `SET XACT_ABORT, NOCOUNT ON;` form DOES turn
    /// XACT_ABORT on; the rule must recognise it and stay silent.
    #[test]
    fn missing_xact_abort_negative_combined_set() {
        let sql = "SET XACT_ABORT, NOCOUNT ON;\nBEGIN TRAN;\n  INSERT INTO dbo.A(x) VALUES (1);\n  UPDATE dbo.B SET y = 2;\nCOMMIT;";
        assert!(!fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    /// Sanity: the comma-combined form with a shared OFF must NOT satisfy the
    /// guard — the rule should still fire.
    #[test]
    fn missing_xact_abort_positive_combined_set_off() {
        let sql = "SET XACT_ABORT, NOCOUNT OFF;\nBEGIN TRAN;\n  INSERT INTO dbo.A(x) VALUES (1);\n  UPDATE dbo.B SET y = 2;\nCOMMIT;";
        assert!(fires(super::dml_batch_missing_xact_abort, sql, Some(2022), "tran.missing_xact_abort"));
    }

    // ---- (d) ddl_inside_explicit_tran -----------------------------------

    #[test]
    fn ddl_in_tran_positive() {
        let sql = "BEGIN TRAN;\n  ALTER TABLE dbo.Orders ADD Note nvarchar(200);\n  UPDATE dbo.Orders SET Note = '';\nCOMMIT;";
        assert!(fires(super::ddl_inside_explicit_tran, sql, Some(2022), "tran.ddl_inside_explicit_tran"));
    }

    #[test]
    fn ddl_in_tran_negative_temp_table() {
        // Creating a #temp table inside a tran is the legitimate idiom.
        let sql = "BEGIN TRAN;\n  CREATE TABLE #staging (id int);\n  INSERT INTO #staging(id) VALUES (1);\nCOMMIT;";
        assert!(!fires(super::ddl_inside_explicit_tran, sql, Some(2022), "tran.ddl_inside_explicit_tran"));
    }

    #[test]
    fn ddl_in_tran_negative_no_tran() {
        // DDL outside any transaction — auto-committed, fine.
        let sql = "ALTER TABLE dbo.Orders ADD Note nvarchar(200);";
        assert!(!fires(super::ddl_inside_explicit_tran, sql, Some(2022), "tran.ddl_inside_explicit_tran"));
    }
}
