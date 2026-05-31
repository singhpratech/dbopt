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

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

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
    if !is_word(&tokens[i], "BEGIN") {
        return None;
    }
    let mut j = next_code(tokens, i + 1);
    // BEGIN DISTRIBUTED TRAN… — still an explicit transaction.
    if j < tokens.len() && is_word(&tokens[j], "DISTRIBUTED") {
        j = next_code(tokens, j + 1);
    }
    if j < tokens.len() && (is_word(&tokens[j], "TRAN") || is_word(&tokens[j], "TRANSACTION")) {
        return Some(j);
    }
    None
}

/// True if index `i` is the start of a COMMIT (`COMMIT` / `COMMIT TRAN…` /
/// `COMMIT WORK`). A bare `COMMIT` is valid T-SQL, so we accept it on its own.
fn is_commit_at(tokens: &[Token<'_>], i: usize) -> bool {
    is_word(&tokens[i], "COMMIT")
}

/// True if index `i` is the start of a ROLLBACK (`ROLLBACK` / `ROLLBACK TRAN…`
/// / `ROLLBACK WORK`). A `SAVE TRAN` + `ROLLBACK <savepoint>` still counts as a
/// rollback path for our purposes.
fn is_rollback_at(tokens: &[Token<'_>], i: usize) -> bool {
    is_word(&tokens[i], "ROLLBACK")
}

/// Whether `BEGIN TRY` appears anywhere in the token stream.
fn has_try_block(tokens: &[Token<'_>]) -> bool {
    for i in 0..tokens.len() {
        if is_word(&tokens[i], "BEGIN") {
            let j = next_code(tokens, i + 1);
            if j < tokens.len() && is_word(&tokens[j], "TRY") {
                return true;
            }
        }
    }
    false
}

/// Whether `SET XACT_ABORT ON` appears anywhere in the token stream. Tolerates
/// comments between the words. We deliberately do NOT match `SET XACT_ABORT
/// OFF` as satisfying the rule.
fn has_xact_abort_on(tokens: &[Token<'_>]) -> bool {
    for i in 0..tokens.len() {
        if !is_word(&tokens[i], "SET") {
            continue;
        }
        let j = next_code(tokens, i + 1);
        if j >= tokens.len() || !is_word(&tokens[j], "XACT_ABORT") {
            continue;
        }
        let k = next_code(tokens, j + 1);
        if k < tokens.len() && is_word(&tokens[k], "ON") {
            return true;
        }
    }
    false
}

/// A DML/DDL statement keyword inside a transaction body. We count these to tell
/// a multi-statement transaction (needs XACT_ABORT) from a single-statement one.
fn is_data_stmt_keyword(t: &Token<'_>) -> bool {
    is_word(t, "INSERT")
        || is_word(t, "UPDATE")
        || is_word(t, "DELETE")
        || is_word(t, "MERGE")
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
        if is_commit_at(tokens, i) || is_rollback_at(tokens, i) {
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
pub fn commit_rollback_without_begin(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let any_begin = (0..tokens.len()).any(|i| begin_tran_at(tokens, i).is_some());
    if any_begin {
        return out;
    }

    // Find the first COMMIT/ROLLBACK and anchor there. SAVE TRAN by itself is not
    // a close, so it won't trip this.
    let mut i = 0;
    while i < tokens.len() {
        let commit = is_commit_at(tokens, i);
        let rollback = is_rollback_at(tokens, i);
        if commit || rollback {
            // Guard: `COMMIT` / `ROLLBACK` could appear as a quoted identifier or
            // string; is_word already excludes strings, and bracketed idents keep
            // their [] so is_word("[COMMIT]","COMMIT") is false. Good enough.
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
        // first COMMIT/ROLLBACK or end-of-stream. Count distinct DML keywords.
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
            if is_data_stmt_keyword(&tokens[j]) {
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
            let verb = if is_word(&tokens[j], "CREATE") {
                Some("CREATE")
            } else if is_word(&tokens[j], "ALTER") {
                Some("ALTER")
            } else if is_word(&tokens[j], "DROP") {
                Some("DROP")
            } else if is_word(&tokens[j], "TRUNCATE") {
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
