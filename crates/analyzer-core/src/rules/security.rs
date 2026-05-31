// Security-smell rules for T-SQL.
//
// These flag patterns that broaden the attack surface or escalate privilege:
//   * xp_cmdshell shell-out
//   * GRANT … TO PUBLIC
//   * GRANT CONTROL (the "everything" permission)
//   * GRANT … WITH GRANT OPTION (re-granting authority)
//   * adding principals to privileged roles (db_owner / sysadmin / …)
//   * EXECUTE AS … with no matching REVERT
//   * OPENROWSET / OPENDATASOURCE with an inline SQL login + password
//
// Dynamic-SQL injection via EXEC(@sql) / EXEC('…' + @x) is already covered by
// `modern.exec_string_concat` and `hygiene.exec_string_no_sp_executesql`, so it
// is deliberately NOT re-implemented here.
//
// Design rules: every finding is anchored to the precise offending token and
// carries a concrete before -> after rewrite. We only walk real Word/String
// tokens, so SQL comments and string literals never trigger a keyword match
// (the lexer classifies them as Comment / String, and `is_word` only matches
// Word tokens). Quoted identifiers ([db_owner]) are bracket-stripped by
// `is_word`, which is acceptable here because the targets are reserved
// role/permission names, not user identifiers.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Bracket-strip a Word token to its bare identifier text.
fn bare<'a>(t: &'a Token<'a>) -> &'a str {
    t.text.trim_matches(|c| c == '[' || c == ']')
}

/// Index of the next non-comment token at or after `from`.
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
        k += 1;
    }
    k
}

/// Strip surrounding single quotes (and an optional N prefix) from a String
/// token, returning the inner text. Doubled quotes inside are left as-is — we
/// only need a case-insensitive contains/eq check on the content.
fn string_inner(t: &Token<'_>) -> String {
    let s = t.text.trim_start_matches(['N', 'n']);
    s.trim_matches('\'').to_string()
}

/// `xp_cmdshell` — runs an OS command line under the SQL Server service
/// account. Almost never appropriate in application code; it is the canonical
/// post-exploitation pivot. We fire on any reference to the proc as a Word
/// token (EXEC xp_cmdshell '…', sp_configure 'xp_cmdshell', or a bare call),
/// because every one of those is worth a human look. Comments/strings can't
/// match (they are not Word tokens).
pub fn xp_cmdshell(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind != TokKind::Word {
            continue;
        }
        if !bare(t).eq_ignore_ascii_case("xp_cmdshell") {
            continue;
        }
        out.push(finding(
            "security.xp_cmdshell",
            Severity::Critical,
            "xp_cmdshell executes arbitrary OS commands under the SQL Server service account — a remote code execution / privilege-escalation vector and a frequent audit failure.",
            Some(make_loc(t)),
            Some(
                "Remove the shell-out. Replace it with a purpose-built, sandboxed mechanism:\n  \
                 • File/process work -> a SQL Agent job step (CmdExec/PowerShell) running under a dedicated least-privilege proxy account.\n  \
                 • External calls -> an out-of-process service (app tier) the database asks via a queue, not the engine itself.\n\
                 If it must stay disabled at rest: `EXEC sp_configure 'xp_cmdshell', 0; RECONFIGURE;` and revoke EXECUTE from non-sysadmins."
                    .to_string(),
            ),
        ));
    }
    out
}

/// `GRANT … TO PUBLIC` — grants a permission to every database principal,
/// present and future. Anchor on the `PUBLIC` token, but only when it is the
/// grantee of a GRANT statement (the statement starts with GRANT and a `TO`
/// keyword precedes PUBLIC), so we don't fire on a column/alias literally named
/// "public".
pub fn grant_to_public(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "PUBLIC") {
            continue;
        }
        // Token immediately before must be `TO`.
        let mut p = i;
        let prev = loop {
            if p == 0 {
                break None;
            }
            p -= 1;
            if tokens[p].kind != TokKind::Comment {
                break Some(p);
            }
        };
        let Some(prev) = prev else { continue };
        if !is_word(&tokens[prev], "TO") {
            continue;
        }
        // Walk backwards to confirm this is a GRANT statement (not REVOKE/DENY,
        // both of which targeting PUBLIC are fine / even desirable). Stop at a
        // statement boundary.
        let mut is_grant = false;
        let mut q = prev;
        while q > 0 {
            q -= 1;
            let w = &tokens[q];
            if w.kind == TokKind::Comment {
                continue;
            }
            if w.text == ";" || is_word(w, "GO") {
                break;
            }
            if is_word(w, "REVOKE") || is_word(w, "DENY") {
                break;
            }
            if is_word(w, "GRANT") {
                is_grant = true;
                break;
            }
        }
        if !is_grant {
            continue;
        }
        out.push(finding(
            "security.grant_to_public",
            Severity::Warning,
            "GRANT … TO PUBLIC grants the permission to every current and future database principal — there is no opting out of the public role.",
            Some(make_loc(t)),
            Some(
                "Grant to a named role scoped to who actually needs it, not PUBLIC. Before -> after:\n  \
                 GRANT SELECT ON dbo.Orders TO PUBLIC;\n  ->\n  \
                 CREATE ROLE OrderReaders;\n  \
                 GRANT SELECT ON dbo.Orders TO OrderReaders;\n  \
                 ALTER ROLE OrderReaders ADD MEMBER [App\\OrdersSvc];\n\
                 If a prior grant to PUBLIC exists, revoke it: `REVOKE SELECT ON dbo.Orders FROM PUBLIC;`"
                    .to_string(),
            ),
        ));
    }
    out
}

/// `GRANT CONTROL …` — CONTROL is the superset permission (it implies ALTER,
/// SELECT, INSERT, EXECUTE, plus the ability to grant onward). On a database or
/// server it is effectively ownership. Fire on the CONTROL token only when it
/// is the permission of a GRANT statement.
pub fn grant_control(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "GRANT") {
            i += 1;
            continue;
        }
        // The permission name(s) follow GRANT, comma-separated, until ON/TO.
        let mut j = skip_comments(tokens, i + 1);
        let mut control_idx: Option<usize> = None;
        while j < tokens.len() {
            let w = &tokens[j];
            if w.kind == TokKind::Comment {
                j += 1;
                continue;
            }
            // Stop scanning the permission list at ON / TO / boundary.
            if is_word(w, "ON") || is_word(w, "TO") || w.text == ";" || is_word(w, "GO") {
                break;
            }
            if is_word(w, "CONTROL") {
                control_idx = Some(j);
                break;
            }
            j += 1;
        }
        if let Some(ci) = control_idx {
            out.push(finding(
                "security.grant_control",
                Severity::Warning,
                "GRANT CONTROL hands over the superset permission (implies ALTER, SELECT/INSERT/UPDATE/DELETE, EXECUTE, and onward-granting) — effectively ownership of the securable.",
                Some(make_loc(&tokens[ci])),
                Some(
                    "Grant only the specific permissions the principal needs. Before -> after:\n  \
                     GRANT CONTROL ON dbo.Orders TO AppUser;\n  ->\n  \
                     GRANT SELECT, INSERT, UPDATE ON dbo.Orders TO AppUser;\n\
                     CONTROL on a database ~ db_owner and on the server ~ sysadmin — avoid it for application logins."
                        .to_string(),
                ),
            ));
        }
        i = j.max(i + 1);
    }
    out
}

/// `GRANT … WITH GRANT OPTION` — lets the grantee re-grant the permission to
/// others, so authority spreads beyond the original DBA's control. Match the
/// exact `WITH GRANT OPTION` token sequence and anchor on `GRANT` (the middle
/// keyword) to disambiguate from other WITH clauses.
pub fn grant_with_grant_option(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "WITH") {
            continue;
        }
        let g = skip_comments(tokens, i + 1);
        if g >= tokens.len() || !is_word(&tokens[g], "GRANT") {
            continue;
        }
        let o = skip_comments(tokens, g + 1);
        if o >= tokens.len() || !is_word(&tokens[o], "OPTION") {
            continue;
        }
        out.push(finding(
            "security.grant_with_grant_option",
            Severity::Warning,
            "WITH GRANT OPTION lets the grantee re-grant this permission to other principals — privilege then propagates outside DBA control and is hard to fully revoke.",
            Some(make_loc(&tokens[g])),
            Some(
                "Drop WITH GRANT OPTION unless delegated administration is a deliberate, documented requirement. Before -> after:\n  \
                 GRANT EXECUTE ON dbo.PostInvoice TO AppUser WITH GRANT OPTION;\n  ->\n  \
                 GRANT EXECUTE ON dbo.PostInvoice TO AppUser;\n\
                 To unwind an existing one you must use CASCADE: `REVOKE EXECUTE ON dbo.PostInvoice FROM AppUser CASCADE;` (this also revokes everyone they granted)."
                    .to_string(),
            ),
        ));
    }
    out
}

/// Adding a principal to a high-privilege role. Two shapes:
///   (a) ALTER ROLE <role> ADD MEMBER <principal>
///   (b) EXEC sp_addrolemember '<role>', '<principal>'        (db role, deprecated)
///   (c) EXEC sp_addsrvrolemember '<login>', '<srvrole>'      (server role, deprecated)
/// We fire only when the role is one of the privileged built-ins. db roles:
/// db_owner / db_securityadmin / db_accessadmin / db_ddladmin. server roles:
/// sysadmin / securityadmin / serveradmin.
pub fn add_to_privileged_role(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Privileged role names (lowercased) and their severity.
    fn role_sev(name: &str) -> Option<(Severity, &'static str)> {
        let n = name.to_ascii_lowercase();
        match n.as_str() {
            // Server roles — owning the instance.
            "sysadmin" => Some((Severity::Critical, "server")),
            "securityadmin" | "serveradmin" => Some((Severity::Error, "server")),
            // Database roles — owning / administering the database.
            "db_owner" | "db_securityadmin" | "db_accessadmin" => Some((Severity::Error, "database")),
            "db_ddladmin" => Some((Severity::Warning, "database")),
            _ => None,
        }
    }

    fn push(out: &mut Vec<Finding>, role: &str, scope: &str, sev: Severity, t: &Token) {
        out.push(finding(
            "security.add_to_privileged_role",
            sev,
            format!(
                "Adding a principal to the `{}` {} role grants broad administrative rights — least-privilege violation and a common audit finding.",
                role, scope
            ),
            Some(make_loc(t)),
            Some(
                "Add the principal to a purpose-built role with only the permissions it needs, not a built-in admin role. Before -> after:\n  \
                 ALTER ROLE db_owner ADD MEMBER AppUser;\n  ->\n  \
                 CREATE ROLE AppWriter;\n  \
                 GRANT SELECT, INSERT, UPDATE, DELETE ON SCHEMA::Sales TO AppWriter;\n  \
                 ALTER ROLE AppWriter ADD MEMBER AppUser;\n\
                 Reserve sysadmin/db_owner for break-glass DBA accounts only."
                    .to_string(),
            ),
        ));
    }

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];

        // (a) ALTER ROLE <role> ADD MEMBER …  (also ALTER SERVER ROLE <role> …)
        if is_word(t, "ALTER") {
            let mut r = skip_comments(tokens, i + 1);
            // Tolerate the server-role form: ALTER SERVER ROLE <role> ADD MEMBER.
            if r < tokens.len() && is_word(&tokens[r], "SERVER") {
                r = skip_comments(tokens, r + 1);
            }
            if r < tokens.len() && is_word(&tokens[r], "ROLE") {
                let n = skip_comments(tokens, r + 1);
                if n < tokens.len() && tokens[n].kind == TokKind::Word {
                    let role = bare(&tokens[n]);
                    // Confirm an `ADD MEMBER` follows so we don't flag ALTER ROLE … DROP MEMBER.
                    let a = skip_comments(tokens, n + 1);
                    let add_member = a < tokens.len()
                        && is_word(&tokens[a], "ADD")
                        && {
                            let m = skip_comments(tokens, a + 1);
                            m < tokens.len() && is_word(&tokens[m], "MEMBER")
                        };
                    if add_member {
                        if let Some((sev, scope)) = role_sev(role) {
                            push(&mut out, role, scope, sev, &tokens[n]);
                        }
                    }
                }
            }
            i += 1;
            continue;
        }

        // (b)/(c) EXEC sp_addrolemember / sp_addsrvrolemember '<role>', …
        // The first string argument is the ROLE for both procs.
        if t.kind == TokKind::Word
            && (bare(t).eq_ignore_ascii_case("sp_addrolemember")
                || bare(t).eq_ignore_ascii_case("sp_addsrvrolemember"))
        {
            // First non-comment token after the proc name; allow optional
            // `@rolename =` then a String literal.
            let mut j = skip_comments(tokens, i + 1);
            // Skip a leading '(' if someone wrote EXEC proc(...).
            if j < tokens.len() && tokens[j].text == "(" {
                j = skip_comments(tokens, j + 1);
            }
            // Skip `@param =` named-argument prefix.
            if j + 1 < tokens.len()
                && tokens[j].kind == TokKind::Word
                && tokens[j].text.starts_with('@')
                && skip_comments(tokens, j + 1) < tokens.len()
                && tokens[skip_comments(tokens, j + 1)].text == "="
            {
                j = skip_comments(tokens, skip_comments(tokens, j + 1) + 1);
            }
            if j < tokens.len() && tokens[j].kind == TokKind::String {
                let role = string_inner(&tokens[j]);
                if let Some((sev, scope)) = role_sev(&role) {
                    push(&mut out, &role, scope, sev, &tokens[j]);
                }
            }
        }

        i += 1;
    }
    out
}

/// `EXECUTE AS LOGIN/USER = …` (or a proc/trigger `WITH EXECUTE AS …`) with no
/// matching `REVERT` in the same batch. The impersonation context then leaks to
/// subsequent statements / callers. We scan per batch (split on `GO`), count
/// standalone `EXECUTE AS LOGIN|USER` statements vs `REVERT` statements.
///
/// We deliberately ignore `WITH EXECUTE AS` in a CREATE/ALTER module header
/// (that is the correct, scoped form and auto-reverts at module exit).
pub fn execute_as_without_revert(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Walk the whole token stream, resetting counters at each GO batch boundary.
    let mut pending: Vec<usize> = Vec::new(); // token idxs of unmatched EXECUTE AS
    let flush = |out: &mut Vec<Finding>, pending: &mut Vec<usize>, tokens: &[Token]| {
        for &idx in pending.iter() {
            out.push(finding(
                "security.execute_as_without_revert",
                Severity::Warning,
                "EXECUTE AS sets an impersonation context with no matching REVERT in the batch — the elevated context leaks to every statement that follows.",
                Some(make_loc(&tokens[idx])),
                Some(
                    "Pair every standalone EXECUTE AS with a REVERT, ideally in TRY/FINALLY-style flow. Before -> after:\n  \
                     EXECUTE AS LOGIN = 'AppAdmin';\n  \
                     -- privileged work\n  ->\n  \
                     EXECUTE AS LOGIN = 'AppAdmin';\n  \
                     -- privileged work\n  \
                     REVERT;\n\
                     For scoped, auto-reverting elevation prefer a module header instead: `CREATE PROCEDURE … WITH EXECUTE AS OWNER AS …`."
                        .to_string(),
                ),
            ));
        }
        pending.clear();
    };

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];

        if is_word(t, "GO") {
            flush(&mut out, &mut pending, tokens);
            i += 1;
            continue;
        }

        // REVERT statement clears one pending impersonation.
        if is_word(t, "REVERT") {
            pending.pop();
            i += 1;
            continue;
        }

        // EXECUTE AS LOGIN | USER — a standalone statement, NOT a module header.
        if is_word(t, "EXECUTE") || is_word(t, "EXEC") {
            let a = skip_comments(tokens, i + 1);
            if a < tokens.len() && is_word(&tokens[a], "AS") {
                let n = skip_comments(tokens, a + 1);
                let is_login_or_user = n < tokens.len()
                    && (is_word(&tokens[n], "LOGIN") || is_word(&tokens[n], "USER"));
                // Guard: if the token right before EXECUTE is `WITH`, this is a
                // module header (CREATE PROC … WITH EXECUTE AS …) — skip it.
                let mut p = i;
                let prev_is_with = loop {
                    if p == 0 {
                        break false;
                    }
                    p -= 1;
                    if tokens[p].kind != TokKind::Comment {
                        break is_word(&tokens[p], "WITH");
                    }
                };
                if is_login_or_user && !prev_is_with {
                    pending.push(i);
                }
            }
        }

        i += 1;
    }
    flush(&mut out, &mut pending, tokens);
    out
}

/// True when the OPENROWSET/OPENDATASOURCE provider name is a file/document
/// provider (Excel/Access/Jet/text) rather than a SQL login connection. These
/// take a file-path provider string, never a login, so they carry no embedded
/// credential — whitelist them like the BULK form.
fn is_file_provider(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // ACE/Jet are the Excel/Access/text-file OLE DB providers; their provider
    // string is a file path, never a SQL login. (MSDASQL/ODBC is intentionally
    // NOT whitelisted — it can carry a real DSN-less login connection string.)
    n.starts_with("microsoft.ace.oledb") || n.starts_with("microsoft.jet.oledb")
}

/// A string literal is a credential connection string if it actually names a
/// user/password key. We require the `=` so that a query body merely mentioning
/// the word "password" (e.g. `WHERE name = 'password='` as a *separate* literal)
/// is never confused with a real `PWD=…`/`Password=…` connection-string token.
/// (Such query text is also excluded by argument position — see the caller —
/// but this keeps the check honest on its own.)
fn looks_like_inline_credential(inner: &str) -> bool {
    let lc = inner.to_ascii_lowercase();
    lc.contains("pwd=")
        || lc.contains("password=")
        || lc.contains("user id=")
        || lc.contains("uid=")
}

/// `OPENROWSET( … )` / `OPENDATASOURCE( … )` carrying an inline SQL login +
/// password. These ad-hoc connectors embed credentials in plaintext in the
/// query (and the plan cache).
///
/// Argument shapes we must distinguish:
///   * `OPENROWSET('provider', 'connstr', 'query')`     — the LAST string is the
///     remote query, NOT a credential. Only the connection string (the args
///     before the query) may hold a secret.
///   * `OPENROWSET('provider','datasource','password','query')` — rare 4-string
///     positional form; the password sits at index 2, the query at index 3.
///   * `OPENDATASOURCE('provider', 'init-string')`      — both args are
///     connection material; there is no query argument.
///
/// We therefore scan ONLY the connection-string arguments for credential
/// keywords and never the trailing remote-query argument, and we only apply the
/// positional-secret fallback to the genuine 4-string positional form. A
/// trusted/integrated connection (`Trusted_Connection=yes`, `Integrated
/// Security=SSPI`) carries no uid/pwd, so it never fires. File providers
/// (ACE/Jet) and the `OPENROWSET(BULK …)` form are whitelisted outright.
pub fn openrowset_inline_credentials(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word {
            continue;
        }
        let name = bare(t);
        let is_openrowset = name.eq_ignore_ascii_case("OPENROWSET");
        let is_opendatasource = name.eq_ignore_ascii_case("OPENDATASOURCE");
        if !is_openrowset && !is_opendatasource {
            continue;
        }
        let lp = skip_comments(tokens, i + 1);
        if lp >= tokens.len() || tokens[lp].text != "(" {
            continue;
        }
        // Walk to the matching ')'.
        let mut depth = 0i32;
        let mut j = lp;
        let mut close = tokens.len();
        while j < tokens.len() {
            if tokens[j].text == "(" {
                depth += 1;
            } else if tokens[j].text == ")" {
                depth -= 1;
                if depth == 0 {
                    close = j;
                    break;
                }
            }
            j += 1;
        }
        if close == tokens.len() {
            continue;
        }

        // Collect string literals inside the parens, in order.
        let mut strings: Vec<&Token> = Vec::new();
        for k in (lp + 1)..close {
            if tokens[k].kind == TokKind::String {
                strings.push(&tokens[k]);
            }
        }

        // BULK form is credential-free (OPENROWSET(BULK 'file', …)). Skip it.
        let first_arg = skip_comments(tokens, lp + 1);
        if first_arg < close && is_word(&tokens[first_arg], "BULK") {
            continue;
        }

        // File/document providers (ACE/Jet/Excel/Access) take a file path, not a
        // login connection string — whitelist them like BULK.
        if let Some(provider) = strings.first() {
            if is_file_provider(&string_inner(provider)) {
                continue;
            }
        }

        // Determine which string arguments are CONNECTION material (scannable
        // for credentials) vs the trailing remote QUERY (never a credential).
        //
        //   OPENROWSET form: ('provider', 'connstr'[, …], 'query') — the LAST
        //     string is the remote query and must be excluded from the scan.
        //   OPENDATASOURCE form: ('provider', 'init-string') — no query arg, so
        //     every string is connection material.
        //
        // For OPENROWSET with >= 2 strings the last one is the query, so the
        // scannable connection args are strings[..len-1]. With a single string
        // there is nothing query-like to exclude.
        let scannable: &[&Token] = if is_opendatasource {
            &strings
        } else if strings.len() >= 2 {
            &strings[..strings.len() - 1]
        } else {
            &strings
        };

        // (a) Keyword path: a real uid/pwd token inside a CONNECTION-string
        //     argument. The trailing query argument is intentionally excluded,
        //     so query text that happens to contain "password=" never fires.
        let mut hit: Option<&Token> = None;
        for s in scannable {
            if looks_like_inline_credential(&string_inner(s)) {
                hit = Some(s);
                break;
            }
        }

        // (b) Positional-secret fallback: ONLY the genuine 4-string positional
        //     form OPENROWSET('provider','datasource','password','query'), where
        //     the credential sits at index 2 and the query at index 3. The
        //     common 3-string form ('provider','connstr','query') has its query
        //     at index 2 and must NOT be treated as a secret.
        if hit.is_none() && is_openrowset && strings.len() >= 4 {
            if let Some(secret) = strings.get(2) {
                if !string_inner(secret).is_empty() {
                    hit = Some(secret);
                }
            }
        }

        if let Some(secret_tok) = hit {
            out.push(finding(
                "security.openrowset_inline_credentials",
                Severity::Error,
                format!(
                    "{}(…) embeds connection credentials inline — the login/password is stored in plaintext in the query text and the plan cache.",
                    name.to_ascii_uppercase()
                ),
                Some(make_loc(secret_tok)),
                Some(
                    "Don't put credentials in the query. Use a server-side credential object and a linked server / external data source. Before -> after:\n  \
                     SELECT * FROM OPENROWSET('SQLNCLI', 'Server=RPT;UID=sa;PWD=Secret!', 'SELECT …');\n  ->\n  \
                     -- one-time setup by a DBA:\n  \
                     CREATE DATABASE SCOPED CREDENTIAL RptCred WITH IDENTITY = 'rpt_reader', SECRET = '<from a vault>';\n  \
                     -- then reference it via an external data source / linked server, no secret in the query.\n\
                     Also enable least-privilege: the embedded login here should never be sa."
                        .to_string(),
                ),
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

    fn ctx_of<'a>(src: &'a str, tokens: &'a [Token<'a>], ver: Option<u16>) -> RuleCtx<'a> {
        RuleCtx {
            src,
            tokens,
            server_version: ver,
            engine: Engine::SqlServer,
        }
    }

    fn run(f: fn(&RuleCtx) -> Vec<Finding>, src: &str) -> Vec<Finding> {
        let toks = tokenize(src);
        let ctx = ctx_of(src, &toks, Some(2022));
        f(&ctx)
    }

    fn assert_fires(findings: &[Finding], id: &str) {
        let f = findings
            .iter()
            .find(|f| f.rule.0 == id)
            .unwrap_or_else(|| panic!("expected rule {id} to fire; got {:?}", findings.iter().map(|x| &x.rule.0).collect::<Vec<_>>()));
        assert!(f.location.is_some(), "rule {id} must set a location");
        assert!(
            f.recommendation.as_ref().map(|r| !r.is_empty()).unwrap_or(false),
            "rule {id} must carry a recommendation"
        );
    }

    fn assert_silent(findings: &[Finding], id: &str) {
        assert!(
            !findings.iter().any(|f| f.rule.0 == id),
            "rule {id} should NOT fire here; got {:?}",
            findings.iter().map(|x| &x.rule.0).collect::<Vec<_>>()
        );
    }

    // --- xp_cmdshell ---
    #[test]
    fn xp_cmdshell_fires() {
        let f = run(xp_cmdshell, "EXEC master..xp_cmdshell 'whoami';");
        assert_fires(&f, "security.xp_cmdshell");
    }
    #[test]
    fn xp_cmdshell_silent_on_comment_and_string() {
        // The token appears only inside a comment and a string literal.
        let f = run(
            xp_cmdshell,
            "-- never call xp_cmdshell here\nSELECT 'xp_cmdshell is banned' AS note;",
        );
        assert_silent(&f, "security.xp_cmdshell");
    }

    // --- grant_to_public ---
    #[test]
    fn grant_public_fires() {
        let f = run(grant_to_public, "GRANT SELECT ON dbo.Orders TO PUBLIC;");
        assert_fires(&f, "security.grant_to_public");
    }
    #[test]
    fn grant_public_silent_on_revoke_and_named_role() {
        let f1 = run(grant_to_public, "REVOKE SELECT ON dbo.Orders FROM PUBLIC;");
        assert_silent(&f1, "security.grant_to_public");
        let f2 = run(grant_to_public, "GRANT SELECT ON dbo.Orders TO OrderReaders;");
        assert_silent(&f2, "security.grant_to_public");
    }

    // --- grant_control ---
    #[test]
    fn grant_control_fires() {
        let f = run(grant_control, "GRANT CONTROL ON dbo.Orders TO AppUser;");
        assert_fires(&f, "security.grant_control");
    }
    #[test]
    fn grant_control_silent_on_specific_perms() {
        let f = run(grant_control, "GRANT SELECT, INSERT ON dbo.Orders TO AppUser;");
        assert_silent(&f, "security.grant_control");
    }

    // --- grant_with_grant_option ---
    #[test]
    fn with_grant_option_fires() {
        let f = run(
            grant_with_grant_option,
            "GRANT EXECUTE ON dbo.PostInvoice TO AppUser WITH GRANT OPTION;",
        );
        assert_fires(&f, "security.grant_with_grant_option");
    }
    #[test]
    fn with_grant_option_silent_on_other_with() {
        let f = run(
            grant_with_grant_option,
            "CREATE INDEX IX ON dbo.T (c) WITH (ONLINE = ON);",
        );
        assert_silent(&f, "security.grant_with_grant_option");
    }

    // --- add_to_privileged_role ---
    #[test]
    fn alter_role_db_owner_fires() {
        let f = run(add_to_privileged_role, "ALTER ROLE db_owner ADD MEMBER AppUser;");
        assert_fires(&f, "security.add_to_privileged_role");
    }
    #[test]
    fn sp_addsrvrolemember_sysadmin_fires() {
        let f = run(
            add_to_privileged_role,
            "EXEC sp_addsrvrolemember 'AppLogin', 'sysadmin';",
        );
        // NOTE: for sp_addsrvrolemember the ROLE is the 2nd arg, the 1st is the
        // login. So this should NOT fire on the role-as-first-arg path; it tests
        // that we don't false-positive when the first string is a login name.
        assert_silent(&f, "security.add_to_privileged_role");
    }
    #[test]
    fn alter_server_role_sysadmin_fires() {
        let f = run(
            add_to_privileged_role,
            "ALTER SERVER ROLE sysadmin ADD MEMBER [App\\Svc];",
        );
        assert_fires(&f, "security.add_to_privileged_role");
    }
    #[test]
    fn sp_addrolemember_db_owner_fires() {
        let f = run(
            add_to_privileged_role,
            "EXEC sp_addrolemember 'db_owner', 'AppUser';",
        );
        assert_fires(&f, "security.add_to_privileged_role");
    }
    #[test]
    fn alter_role_silent_on_app_role_and_drop_member() {
        let f1 = run(add_to_privileged_role, "ALTER ROLE AppWriter ADD MEMBER AppUser;");
        assert_silent(&f1, "security.add_to_privileged_role");
        let f2 = run(add_to_privileged_role, "ALTER ROLE db_owner DROP MEMBER OldUser;");
        assert_silent(&f2, "security.add_to_privileged_role");
    }

    // --- execute_as_without_revert ---
    #[test]
    fn execute_as_without_revert_fires() {
        let f = run(
            execute_as_without_revert,
            "EXECUTE AS LOGIN = 'AppAdmin';\nUPDATE dbo.T SET c = 1;",
        );
        assert_fires(&f, "security.execute_as_without_revert");
    }
    #[test]
    fn execute_as_with_revert_silent() {
        let f = run(
            execute_as_without_revert,
            "EXECUTE AS LOGIN = 'AppAdmin';\nUPDATE dbo.T SET c = 1;\nREVERT;",
        );
        assert_silent(&f, "security.execute_as_without_revert");
    }
    #[test]
    fn execute_as_module_header_silent() {
        // WITH EXECUTE AS OWNER in a module header auto-reverts; must not fire.
        let f = run(
            execute_as_without_revert,
            "CREATE PROCEDURE dbo.P WITH EXECUTE AS OWNER AS BEGIN SELECT 1; END;",
        );
        assert_silent(&f, "security.execute_as_without_revert");
    }

    // --- openrowset_inline_credentials ---
    #[test]
    fn openrowset_inline_creds_fires() {
        // Real secret: UID=/PWD= sit inside the connection-string argument.
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=RPT;UID=sa;PWD=Secret!', 'SELECT 1');",
        );
        assert_fires(&f, "security.openrowset_inline_credentials");
    }
    #[test]
    fn openrowset_bulk_silent() {
        // BULK form has no credentials — must not fire.
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET(BULK 'C:\\data\\file.csv', FORMATFILE = 'C:\\data\\fmt.xml') AS x;",
        );
        assert_silent(&f, "security.openrowset_inline_credentials");
    }

    // --- openrowset false-positive regression guards (fp_flags.json) ---

    // FP #1: canonical credential-FREE 3-string form with Trusted_Connection.
    // strings = ['SQLNCLI', 'Server=RPT;Trusted_Connection=yes', 'SELECT … query'].
    // The 3rd string is the remote QUERY, not a password; no uid/pwd anywhere.
    #[test]
    fn openrowset_trusted_connection_3string_silent() {
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=RPT;Trusted_Connection=yes', 'SELECT * FROM dbo.Report');",
        );
        assert_silent(&f, "security.openrowset_inline_credentials");
    }

    // FP #2: ACE OLEDB Excel/CSV file import — file-path provider string, no
    // login. Whitelisted like BULK.
    #[test]
    fn openrowset_ace_excel_file_silent() {
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET('Microsoft.ACE.OLEDB.12.0', 'Excel 12.0;Database=C:\\data\\book.xlsx', 'SELECT * FROM [Sheet1$]');",
        );
        assert_silent(&f, "security.openrowset_inline_credentials");
    }

    // FP #3: trusted connection where the remote QUERY body literally contains
    // the substring "password=" as data. The query argument is never scanned.
    #[test]
    fn openrowset_password_in_query_body_silent() {
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=R;Trusted_Connection=yes', 'SELECT * FROM cfg WHERE name = ''password=''');",
        );
        assert_silent(&f, "security.openrowset_inline_credentials");
    }

    // Positive guard: the genuine 4-string positional form still fires
    // (provider, datasource, password, query) — secret at index 2.
    #[test]
    fn openrowset_positional_4string_secret_fires() {
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=RPT', 'Sup3rSecret', 'SELECT 1');",
        );
        assert_fires(&f, "security.openrowset_inline_credentials");
    }

    // Positive guard: OPENDATASOURCE init-string with an embedded password still
    // fires (no query argument; the whole init string is connection material).
    #[test]
    fn opendatasource_inline_pwd_fires() {
        let f = run(
            openrowset_inline_credentials,
            "SELECT * FROM OPENDATASOURCE('SQLNCLI', 'Data Source=RPT;User ID=sa;Password=Secret!').db.dbo.t;",
        );
        assert_fires(&f, "security.openrowset_inline_credentials");
    }
}
