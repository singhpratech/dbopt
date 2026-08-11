// Offline missing-index inference from query SHAPE alone (no DMVs / no plan).
//
// This is the analyzer's flagship offline USP: given a single-table SELECT with
// WHERE equality / range predicates, we emit a concrete, copy-paste
// `CREATE NONCLUSTERED INDEX ...` whose key order follows the SARGable rule
// (equality columns first, then a single range column last) and whose INCLUDE
// list covers the projected columns. We also flag ORDER-BY-driven sorts and
// key-lookup risk where a covering index removes the work.
//
// FALSE POSITIVES ARE THE WORST OUTCOME. Every rule here bails out the moment
// the statement shape is anything other than a textbook single-base-table
// SELECT we can read with total confidence:
//   * any JOIN / APPLY / comma-FROM (multiple tables)        -> skip
//   * derived table / subquery / CTE / table-valued-function -> skip
//   * a table name we cannot extract as a plain identifier   -> skip
//   * a predicate column we cannot extract as a bare column  -> skip
//   * SELECT *                                               -> skip the INCLUDE-bearing rules
// When unsure we drop the finding rather than emit noise.

use super::{is_keyword, finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

/// Strip surrounding [] brackets (the lexer keeps `[col]` as one Word token).
fn bare<'a>(t: &'a Token<'a>) -> &'a str {
    t.text.trim_matches(|c| c == '[' || c == ']')
}

fn name_eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

/// Next non-comment index at or after `from`.
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment { k += 1; }
    k
}

/// A column reference extracted from a predicate / order-by / select list:
/// the bare (bracket-stripped) column name plus the token index of the part we
/// want to anchor a finding at (the column identifier itself).
#[derive(Clone)]
struct ColRef {
    name: String,
    tok: usize,
}

/// One single-table SELECT statement we are confident enough to reason about.
struct SelectStmt {
    /// Token index of the SELECT keyword (for anchoring).
    select_tok: usize,
    /// Verbatim table reference text as it should appear in DDL, e.g. `dbo.Orders`.
    table_ref: String,
    /// `true` if the projection is `SELECT *` / `SELECT a.*`.
    select_star: bool,
    /// Projected column names (best-effort, bare). Empty when `select_star`.
    select_cols: Vec<String>,
    /// Equality-predicate columns from the WHERE clause, in source order.
    eq_cols: Vec<ColRef>,
    /// Range-predicate columns (<,>,<=,>=,BETWEEN) from the WHERE clause.
    range_cols: Vec<ColRef>,
    /// ORDER BY columns (bare), in source order. Empty when no ORDER BY.
    order_cols: Vec<ColRef>,
}

/// Walk the whole token stream and return every single-base-table SELECT we can
/// confidently model. Anything ambiguous is silently skipped.
/// Names bound by `WITH <name> AS ( ... )`. A CTE is not a table: you cannot
/// `CREATE INDEX` on it, and its columns may be computed window values that do
/// not exist anywhere on disk. Emitting index DDL against one produces a script
/// that fails to parse — the worst kind of copy-paste advice.
/// Keyed by batch, because a CTE does not survive `GO`. A file-wide set let a
/// CTE in one batch suppress index advice for a *real* table of the same name
/// in every later batch — silence on exactly the query the rule exists for.
fn cte_names(tokens: &[Token<'_>]) -> std::collections::HashSet<(u32, String)> {
    let mut names = std::collections::HashSet::new();
    let mut batch = 0u32;
    for (i, t) in tokens.iter().enumerate() {
        if is_keyword(t, "GO") {
            batch += 1;
            continue;
        }
        if !(is_word(t, "WITH") || t.text == ",") {
            continue;
        }
        let Some(name) = tokens.get(i + 1) else { continue };
        if name.kind != TokKind::Word {
            continue;
        }
        let is_cte = tokens.get(i + 2).map(|n| is_word(n, "AS")).unwrap_or(false)
            && tokens.get(i + 3).map(|n| n.text == "(").unwrap_or(false);
        if is_cte {
            names.insert((batch, name.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase()));
        }
    }
    names
}

fn parse_single_table_selects(tokens: &[Token<'_>]) -> Vec<SelectStmt> {
    let ctes = cte_names(tokens);
    let mut out = Vec::new();
    let mut i = 0;
    let mut batch = 0u32;
    while i < tokens.len() {
        if is_keyword(&tokens[i], "GO") { batch += 1; i += 1; continue; }
        if !is_word(&tokens[i], "SELECT") { i += 1; continue; }
        let select_tok = i;

        // Statement boundary: top-level ';' or end of input. Parens are tracked
        // so a `;` inside a (sub)expression doesn't end us early.
        let mut depth = 0i32;
        let mut stmt_end = tokens.len();
        let mut j = i + 1;
        while j < tokens.len() {
            let t = &tokens[j];
            if t.text == "(" { depth += 1; }
            else if t.text == ")" { depth -= 1; if depth < 0 { stmt_end = j; break; } }
            else if depth == 0 && t.text == ";" { stmt_end = j; break; }
            j += 1;
        }

        if let Some(stmt) = parse_one(tokens, select_tok, stmt_end) {
            let short = table_short_name(&stmt.table_ref).to_ascii_lowercase();
            // System catalog views and DMVs are not yours to index; emitting
            // `CREATE INDEX ON sys.indexes` is DDL nobody can run.
            let schema = stmt.table_ref.to_ascii_lowercase();
            let system_object = schema.starts_with("sys.")
                || schema.starts_with("[sys].")
                || schema.starts_with("information_schema.")
                || short.starts_with("dm_");
            if !ctes.contains(&(batch, short)) && !system_object {
                out.push(stmt);
            }
        }
        i = stmt_end + 1;
    }
    out
}

/// Locate top-level (depth-0) clause keyword indices inside [start, end).
struct Clauses {
    from: Option<usize>,
    where_: Option<usize>,
    group: Option<usize>,
    order: Option<usize>,
    having: Option<usize>,
    option: Option<usize>,
}

fn find_clauses(tokens: &[Token<'_>], start: usize, end: usize) -> Clauses {
    let mut c = Clauses { from: None, where_: None, group: None, order: None, having: None, option: None };
    let mut depth = 0i32;
    let mut k = start;
    while k < end {
        let t = &tokens[k];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.kind == TokKind::Word {
            if c.from.is_none() && is_word(t, "FROM") { c.from = Some(k); }
            else if c.where_.is_none() && is_word(t, "WHERE") { c.where_ = Some(k); }
            else if c.group.is_none() && is_word(t, "GROUP") { c.group = Some(k); }
            else if c.order.is_none() && is_word(t, "ORDER") { c.order = Some(k); }
            else if c.having.is_none() && is_word(t, "HAVING") { c.having = Some(k); }
            else if c.option.is_none() && is_word(t, "OPTION") { c.option = Some(k); }
        }
        k += 1;
    }
    c
}

fn parse_one(tokens: &[Token<'_>], select_tok: usize, stmt_end: usize) -> Option<SelectStmt> {
    let clauses = find_clauses(tokens, select_tok + 1, stmt_end);
    let from_tok = clauses.from?; // need a FROM to have a table

    // ---- FROM clause: exactly one plain base table, nothing else ---------
    // The "from body" runs from just after FROM up to the next clause keyword.
    let from_body_end = [clauses.where_, clauses.group, clauses.order, clauses.having, clauses.option]
        .into_iter()
        .flatten()
        .filter(|&x| x > from_tok)
        .min()
        .unwrap_or(stmt_end);

    let (table_ref, _table_tok) = extract_single_table(tokens, from_tok + 1, from_body_end)?;

    // ---- projection ------------------------------------------------------
    let (select_star, select_cols) = parse_projection(tokens, select_tok + 1, from_tok);

    // ---- WHERE predicates ------------------------------------------------
    let (eq_cols, range_cols) = if let Some(w) = clauses.where_ {
        let where_end = [clauses.group, clauses.order, clauses.having, clauses.option]
            .into_iter()
            .flatten()
            .filter(|&x| x > w)
            .min()
            .unwrap_or(stmt_end);
        parse_where_predicates(tokens, w + 1, where_end)
    } else {
        (Vec::new(), Vec::new())
    };

    // ---- ORDER BY columns ------------------------------------------------
    let order_cols = if let Some(o) = clauses.order {
        let order_end = [clauses.option]
            .into_iter()
            .flatten()
            .filter(|&x| x > o)
            .min()
            .unwrap_or(stmt_end);
        parse_order_by(tokens, o, order_end)
    } else {
        Vec::new()
    };

    Some(SelectStmt {
        select_tok,
        table_ref,
        select_star,
        select_cols,
        eq_cols,
        range_cols,
        order_cols,
    })
}

/// Read the FROM body and return the single base table reference, or None if the
/// shape is anything we can't model with total confidence (join, subquery,
/// comma list, TVF, hint, variable, etc.).
fn extract_single_table(tokens: &[Token<'_>], start: usize, end: usize) -> Option<(String, usize)> {
    let first = skip_comments(tokens, start);
    if first >= end { return None; }

    // A derived table / subquery starts with '(' — bail.
    if tokens[first].text == "(" { return None; }
    // A table variable (@t) or temp (#t) — the lexer keeps the sigil in-token.
    if tokens[first].kind != TokKind::Word { return None; }
    let lead = tokens[first].text;
    if lead.starts_with('@') || lead.starts_with('#') { return None; }

    // Collect the dotted identifier: Word (. Word)*  e.g. db.dbo.Orders / dbo.Orders / Orders.
    let mut parts: Vec<usize> = vec![first];
    let mut k = first + 1;
    loop {
        let dot = skip_comments(tokens, k);
        if dot < end && tokens[dot].text == "." {
            let nxt = skip_comments(tokens, dot + 1);
            if nxt < end && tokens[nxt].kind == TokKind::Word && tokens[nxt].text != "(" {
                parts.push(nxt);
                k = nxt + 1;
                continue;
            }
            return None; // dangling dot — give up rather than guess
        }
        k = dot;
        break;
    }
    if parts.len() > 3 { return None; } // server.db.schema.table is too exotic to model

    // Whatever follows the identifier must be only: an optional alias and
    // nothing that signals multiplicity (JOIN/APPLY/comma) or a TVF call '('.
    let mut p = skip_comments(tokens, k);
    // optional AS
    if p < end && is_word(&tokens[p], "AS") {
        p = skip_comments(tokens, p + 1);
        // alias word required after AS
        if p < end && tokens[p].kind == TokKind::Word { p = skip_comments(tokens, p + 1); }
        else { return None; }
    } else if p < end && tokens[p].kind == TokKind::Word {
        // bare alias — but reject anything that is actually a join/clause keyword
        let w = tokens[p].text;
        if is_join_or_break_kw(w) { /* not an alias, that's fine */ }
        else { p = skip_comments(tokens, p + 1); }
    }

    // After the (optional) alias the FROM body must be exhausted. If anything
    // remains — a comma (second table), JOIN/APPLY, a '(' (TVF args), a WITH
    // hint, etc. — we are not in single-table territory.
    let rest = skip_comments(tokens, p);
    if rest < end {
        return None;
    }

    // Reject TVF: `dbo.fn(...)` — the part right after the identifier is '('.
    // (Handled above because '(' at `rest` would be inside [start,end); but a
    // TVF with no alias would leave '(' as the first post-ident token, which we
    // catch here too.)
    let after_ident = skip_comments(tokens, k);
    if after_ident < end && tokens[after_ident].text == "(" { return None; }

    // Build the verbatim reference text from the identifier parts (preserve the
    // user's bracketing/casing so the generated DDL is paste-ready).
    let mut s = String::new();
    for (idx, &pi) in parts.iter().enumerate() {
        if idx > 0 { s.push('.'); }
        s.push_str(tokens[pi].text);
    }
    // The table identifier we anchor at is the LAST part (the actual name).
    Some((s, *parts.last().unwrap()))
}

fn is_join_or_break_kw(w: &str) -> bool {
    const KWS: &[&str] = &[
        "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER", "APPLY",
        "WHERE", "GROUP", "ORDER", "HAVING", "OPTION", "UNION", "ON", "PIVOT", "UNPIVOT",
    ];
    KWS.iter().any(|k| name_eq_ci(w, k))
}

/// Parse the projection list between SELECT and FROM. Returns (is_star, cols).
/// Columns are only collected when each item is a *plain* column reference
/// (`col`, `t.col`, optionally `[col]`); any expression / function / literal /
/// `*` makes us return star=false but stop collecting (empty list) so we never
/// fabricate an INCLUDE list from something we didn't understand.
fn parse_projection(tokens: &[Token<'_>], start: usize, from_tok: usize) -> (bool, Vec<String>) {
    // Skip an optional DISTINCT / TOP (n) [PERCENT] prefix.
    let mut p = skip_comments(tokens, start);
    if p < from_tok && is_word(&tokens[p], "DISTINCT") { p = skip_comments(tokens, p + 1); }
    if p < from_tok && is_word(&tokens[p], "TOP") {
        p = skip_comments(tokens, p + 1);
        if p < from_tok && tokens[p].text == "(" {
            let mut d = 0i32;
            while p < from_tok {
                if tokens[p].text == "(" { d += 1; }
                else if tokens[p].text == ")" { d -= 1; if d == 0 { p += 1; break; } }
                p += 1;
            }
        } else if p < from_tok && tokens[p].kind == TokKind::Number {
            p = skip_comments(tokens, p + 1);
        }
        if p < from_tok && is_word(&tokens[p], "PERCENT") { p = skip_comments(tokens, p + 1); }
        if p < from_tok && is_word(&tokens[p], "WITH") {
            // TOP ... WITH TIES — skip two words conservatively.
            p = skip_comments(tokens, p + 1);
            if p < from_tok && is_word(&tokens[p], "TIES") { p = skip_comments(tokens, p + 1); }
        }
    }

    // Split into top-level comma items.
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0i32;
    let mut item_start = p;
    let mut k = p;
    while k < from_tok {
        let t = &tokens[k];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.text == "," { items.push((item_start, k)); item_start = k + 1; }
        k += 1;
    }
    if item_start < from_tok { items.push((item_start, from_tok)); }

    let mut cols: Vec<String> = Vec::new();
    for (s, e) in items {
        let a = skip_comments(tokens, s);
        if a >= e { return (false, Vec::new()); }
        // `*` or `alias.*`
        if tokens[a].text == "*" { return (true, Vec::new()); }
        if a + 2 < e && tokens[a].kind == TokKind::Word && tokens[a + 1].text == "." && tokens[a + 2].text == "*" {
            return (true, Vec::new());
        }
        // A plain column item is: Word (. Word)? optionally followed by `AS alias` / `alias`.
        // Anything else (function call, arithmetic, literal, CASE, subquery) means
        // we don't understand the projection — return cols we can't trust as empty.
        let col = match plain_column_at(tokens, a, e) {
            Some(c) => c,
            None => return (false, Vec::new()),
        };
        cols.push(col);
    }
    (false, cols)
}

/// If [start,end) is exactly a plain column reference (optionally with an alias),
/// return the bare column name. Otherwise None.
fn plain_column_at(tokens: &[Token<'_>], start: usize, end: usize) -> Option<String> {
    let a = skip_comments(tokens, start);
    if a >= end || tokens[a].kind != TokKind::Word { return None; }
    if tokens[a].text.starts_with('@') { return None; } // variable, not a column
    // Optional `.col` qualifier (take the last segment as the column).
    let mut col_tok = a;
    let mut k = a + 1;
    let d1 = skip_comments(tokens, k);
    if d1 < end && tokens[d1].text == "." {
        let nxt = skip_comments(tokens, d1 + 1);
        if nxt < end && tokens[nxt].kind == TokKind::Word {
            col_tok = nxt;
            k = nxt + 1;
        } else {
            return None;
        }
    }
    // Whatever remains may only be an alias: `AS name` or a bare `name`.
    let mut p = skip_comments(tokens, k);
    if p < end && is_word(&tokens[p], "AS") {
        p = skip_comments(tokens, p + 1);
        if p < end && tokens[p].kind == TokKind::Word { p = skip_comments(tokens, p + 1); } else { return None; }
    } else if p < end && tokens[p].kind == TokKind::Word {
        p = skip_comments(tokens, p + 1);
    }
    if skip_comments(tokens, p) < end { return None; } // trailing junk -> not a plain column
    Some(bare(&tokens[col_tok]).to_string())
}

/// Parse WHERE predicates into (equality cols, range cols). We only model a
/// conjunction of simple `col <op> <literal-or-param>` comparisons joined by
/// AND at depth 0. The presence of any OR (at depth 0) makes the index
/// recommendation unsound, so we bail entirely. Function-wrapped columns,
/// column-to-column comparisons, and IN/EXISTS subqueries are ignored (not
/// errors — just not modeled as seekable keys).
fn parse_where_predicates(tokens: &[Token<'_>], start: usize, end: usize) -> (Vec<ColRef>, Vec<ColRef>) {
    // Bail on ANY OR, at ANY paren depth — a disjunction anywhere in the WHERE
    // makes a single-seek-key recommendation unsound. A parenthesized OR-group
    // (`A = 1 AND (B = 2 OR C = 3)`) is depth-1, so a depth-0-only scan would
    // miss it and the conjunct splitter would silently truncate `(B = 2 OR C =
    // 3)` to a bare `B = 2` equality, fabricating an index key on a column that
    // only appears inside a disjunction. Scanning every depth closes that class.
    let mut k = start;
    while k < end {
        if is_word(&tokens[k], "OR") { return (Vec::new(), Vec::new()); }
        k += 1;
    }

    let mut eq: Vec<ColRef> = Vec::new();
    let mut range: Vec<ColRef> = Vec::new();

    // Split into depth-0 AND-separated conjuncts.
    let mut conj: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0i32;
    let mut cstart = start;
    k = start;
    while k < end {
        let t = &tokens[k];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && is_word(t, "AND") { conj.push((cstart, k)); cstart = k + 1; }
        k += 1;
    }
    if cstart < end { conj.push((cstart, end)); }

    for (cs, ce) in conj {
        if let Some((col, is_range)) = parse_simple_predicate(tokens, cs, ce) {
            if is_range {
                if !range.iter().any(|c| name_eq_ci(&c.name, &col.name))
                    && !eq.iter().any(|c| name_eq_ci(&c.name, &col.name))
                {
                    range.push(col);
                }
            } else if !eq.iter().any(|c| name_eq_ci(&c.name, &col.name))
                && !range.iter().any(|c| name_eq_ci(&c.name, &col.name))
            {
                eq.push(col);
            }
        }
    }
    (eq, range)
}

/// Parse one conjunct. Returns (column, is_range) only when it is a clean
/// `<col> <op> <constant/param>` (or `<col> BETWEEN ...`). Strips outer parens.
fn parse_simple_predicate(tokens: &[Token<'_>], start: usize, end: usize) -> Option<(ColRef, bool)> {
    let mut a = skip_comments(tokens, start);
    let mut e = end;
    // Strip a single layer of wrapping parens: ( <pred> ).
    while a < e {
        let aa = skip_comments(tokens, a);
        if aa < e && tokens[aa].text == "(" {
            // Find matching ')'. If it closes exactly at e-1, unwrap.
            let mut d = 0i32;
            let mut m = aa;
            let mut close = None;
            while m < e {
                if tokens[m].text == "(" { d += 1; }
                else if tokens[m].text == ")" { d -= 1; if d == 0 { close = Some(m); break; } }
                m += 1;
            }
            match close {
                Some(c) if c + 1 >= e => { a = aa + 1; e = c; }
                _ => break,
            }
        } else { break; }
    }

    let lhs = skip_comments(tokens, a);
    if lhs >= e || tokens[lhs].kind != TokKind::Word { return None; }
    if tokens[lhs].text.starts_with('@') { return None; } // @var on the left

    // The LHS must be a *bare* column (optionally `alias.col`) and NOT a
    // function call. If the token after the identifier is '(', it's a function.
    let mut col_tok = lhs;
    let mut k = lhs + 1;
    let d1 = skip_comments(tokens, k);
    if d1 < e && tokens[d1].text == "." {
        let nxt = skip_comments(tokens, d1 + 1);
        if nxt < e && tokens[nxt].kind == TokKind::Word {
            col_tok = nxt;
            k = nxt + 1;
        } else {
            return None;
        }
    }
    let after = skip_comments(tokens, k);
    if after >= e { return None; }
    // Function call on the LHS -> non-SARGable, not a clean key column.
    if tokens[after].text == "(" { return None; }

    let op = &tokens[after];
    let col = ColRef { name: bare(&tokens[col_tok]).to_string(), tok: col_tok };

    // BETWEEN -> range.
    if is_word(op, "BETWEEN") {
        return Some((col, true));
    }
    // IN -> treat as equality-class (can seek), but only for a literal list, not
    // a subquery. Be conservative: require '(' then no SELECT inside.
    if is_word(op, "IN") {
        let lp = skip_comments(tokens, after + 1);
        if lp < e && tokens[lp].text == "(" {
            // ensure no SELECT inside (subquery) -> then it's a value list.
            let mut d = 0i32;
            let mut m = lp;
            let mut has_select = false;
            while m < e {
                if tokens[m].text == "(" { d += 1; }
                else if tokens[m].text == ")" { d -= 1; if d == 0 { break; } }
                else if is_word(&tokens[m], "SELECT") { has_select = true; }
                m += 1;
            }
            if !has_select { return Some((col, false)); }
        }
        return None;
    }

    // Comparison operators. The lexer emits each punct char separately, so `<=`
    // is `<` then `=`. Treat `=` as equality; `<`,`>` (with/without `=`) as range.
    let op_txt = op.text;
    let rhs_start;
    let is_range;
    match op_txt {
        "=" => { is_range = false; rhs_start = after + 1; }
        "<" | ">" => {
            // peek for a following '=' or '>' (>=, <=, <>) — still range, except
            // `<>`/`!=` (inequality) which is NOT seekable -> reject.
            let nxt = skip_comments(tokens, after + 1);
            if nxt < e && tokens[nxt].text == ">" { return None; } // `<>`
            is_range = true;
            rhs_start = after + 1;
        }
        "!" => return None, // `!=` / `!<` etc. — not a clean seek
        _ => return None,
    }

    // RHS must be a constant / parameter / simple value — NOT another column.
    // We require the RHS to start with a literal (Number/String) or @param or a
    // function like GETDATE()/N'..'. If RHS is a bare Word that isn't a param,
    // it's probably a column-to-column comparison -> reject (can't index that).
    let r = skip_comments(tokens, rhs_start);
    if r >= e { return None; }
    let rt = &tokens[r];
    let rhs_ok = matches!(rt.kind, TokKind::Number | TokKind::String)
        || (rt.kind == TokKind::Word && rt.text.starts_with('@'))
        || (rt.kind == TokKind::Word && rt.text.eq_ignore_ascii_case("N")) // N'...'
        || (rt.kind == TokKind::Word && is_known_constant_fn(rt.text));
    if !rhs_ok { return None; }

    // End-of-conjunct assertion: the matched `col <op> <value>` must consume the
    // ENTIRE conjunct. Anything left over means this wasn't a clean atomic
    // predicate — e.g. a parenthesized OR-leg (`(B = 2 OR C = 3)` unwrapped to
    // `B = 2 OR C = 3`), an arithmetic RHS (`col = 1 + 2`), `col = @x OR ...`,
    // or other trailing junk. In every such case the column is NOT a sound
    // single-seek key, so we drop the predicate rather than truncate it.
    let value_end = rhs_value_end(tokens, r, e);
    if skip_comments(tokens, value_end) < e { return None; }

    Some((col, is_range))
}

/// Given the first token of an RHS value at `start`, return the index just past
/// the complete value token(s). A bare literal / @param / single word is one
/// token. `N'..'` is the `N` word followed by a String. A constant function
/// like `GETDATE()` / `DATEADD(day,1,@x)` spans the word plus its balanced
/// `( ... )` argument list.
fn rhs_value_end(tokens: &[Token<'_>], start: usize, end: usize) -> usize {
    let r = skip_comments(tokens, start);
    if r >= end { return end; }
    let rt = &tokens[r];
    // N'...': consume the `N` then the adjacent String literal.
    if rt.kind == TokKind::Word && rt.text.eq_ignore_ascii_case("N") {
        let s = skip_comments(tokens, r + 1);
        if s < end && tokens[s].kind == TokKind::String { return s + 1; }
        return r + 1;
    }
    // A word that is immediately followed by '(' is a function call -> consume
    // its balanced parenthesis group (e.g. GETDATE(), DATEADD(...)).
    if rt.kind == TokKind::Word {
        let lp = skip_comments(tokens, r + 1);
        if lp < end && tokens[lp].text == "(" {
            let mut d = 0i32;
            let mut m = lp;
            while m < end {
                if tokens[m].text == "(" { d += 1; }
                else if tokens[m].text == ")" { d -= 1; if d == 0 { return m + 1; } }
                m += 1;
            }
            return end; // unbalanced -> treat as consumed to the conjunct end
        }
    }
    // Plain single-token value (Number / String / @param / bare word).
    r + 1
}

/// RHS function-ish words we accept as "a constant value" so e.g.
/// `created < GETDATE()` is still modeled. Conservative allowlist.
///
/// NOTE: NULL is deliberately NOT here. `Col = NULL` is the constant UNKNOWN
/// under SET ANSI_NULLS ON and matches zero rows, so recommending an index to
/// accelerate a predicate that can never return a row is a false positive
/// (it's a code smell handled by a sargability/ANSI_NULLS rule, not by us).
fn is_known_constant_fn(w: &str) -> bool {
    const FNS: &[&str] = &["GETDATE", "GETUTCDATE", "SYSDATETIME", "SYSUTCDATETIME", "DATEADD"];
    FNS.iter().any(|f| name_eq_ci(w, f))
}

/// Parse ORDER BY columns. Each item must be a plain column (optionally with
/// `ASC`/`DESC`); if any item isn't, we return empty (don't model the sort).
fn parse_order_by(tokens: &[Token<'_>], order_tok: usize, end: usize) -> Vec<ColRef> {
    // ORDER must be followed by BY.
    let by = skip_comments(tokens, order_tok + 1);
    if by >= end || !is_word(&tokens[by], "BY") { return Vec::new(); }

    let list_start = by + 1;
    // Split on top-level commas.
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0i32;
    let mut s = list_start;
    let mut k = list_start;
    while k < end {
        let t = &tokens[k];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.text == "," { items.push((s, k)); s = k + 1; }
        k += 1;
    }
    if s < end { items.push((s, end)); }

    let mut cols = Vec::new();
    for (is_, ie) in items {
        let a = skip_comments(tokens, is_);
        if a >= ie || tokens[a].kind != TokKind::Word { return Vec::new(); }
        // Ordinal ORDER BY (1,2) -> can't map to columns; bail.
        if tokens[a].kind == TokKind::Number { return Vec::new(); }
        let mut col_tok = a;
        let mut p = a + 1;
        let d1 = skip_comments(tokens, p);
        if d1 < ie && tokens[d1].text == "." {
            let nxt = skip_comments(tokens, d1 + 1);
            if nxt < ie && tokens[nxt].kind == TokKind::Word { col_tok = nxt; p = nxt + 1; } else { return Vec::new(); }
        }
        // A trailing function-call '(' means an expression sort -> bail.
        let q = skip_comments(tokens, p);
        if q < ie && tokens[q].text == "(" { return Vec::new(); }
        // Allow optional ASC/DESC, otherwise it must be the end of the item.
        if q < ie {
            if is_word(&tokens[q], "ASC") || is_word(&tokens[q], "DESC") {
                if skip_comments(tokens, q + 1) < ie { return Vec::new(); }
            } else {
                return Vec::new();
            }
        }
        cols.push(ColRef { name: bare(&tokens[col_tok]).to_string(), tok: col_tok });
    }
    cols
}

/// Derive a safe identifier fragment for the index name from a column name.
fn ident_frag(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
}

/// An index the batch itself declares: `CREATE [UNIQUE] [NON]CLUSTERED INDEX
/// <name> ON <table> (k1, k2 …) [INCLUDE (c1, c2 …)]`.
///
/// Two rules below need this. `missing_index_from_predicate` claimed "no
/// matching index is declared in this batch" while never looking, and
/// `key_lookup_risk` re-emitted the same DDL under the same generated name — so
/// a batch could hand you two `CREATE NONCLUSTERED INDEX [IX_Orders_Status]`
/// statements, the second of which fails. Reading the batch's own DDL makes the
/// first claim true and gives the two rules a real seam: no index declared →
/// recommend one (covering); index declared but not covering → warn about the
/// lookup. They can no longer both speak about the same table.
struct DeclaredIndex {
    name: String,
    table: String,
    key_cols: Vec<String>,
    include_cols: Vec<String>,
}

impl DeclaredIndex {
    /// Does this index give a seek on `leading`? Only the leading key column can
    /// be seeked without the caller supplying the earlier keys.
    fn leads_with(&self, leading: &str) -> bool {
        self.key_cols.first().is_some_and(|k| name_eq_ci(k, leading))
    }

    /// Are all of `cols` already retrievable from the index (key or INCLUDE)?
    fn covers(&self, cols: &[String]) -> bool {
        cols.iter().all(|c| {
            self.key_cols.iter().any(|k| name_eq_ci(k, c))
                || self.include_cols.iter().any(|i| name_eq_ci(i, c))
        })
    }
}

/// Collect a parenthesised, comma-separated column list starting at `open`
/// (the index of the `(`). Returns the columns and the index just past `)`.
/// Bails to `None` on anything that isn't a flat list of bare identifiers.
fn column_list(tokens: &[Token<'_>], open: usize) -> Option<(Vec<String>, usize)> {
    if tokens.get(open).map(|t| t.text) != Some("(") { return None; }
    let mut cols = Vec::new();
    let mut k = skip_comments(tokens, open + 1);
    loop {
        let t = tokens.get(k)?;
        if t.kind != TokKind::Word { return None; }
        cols.push(bare(t).to_string());
        k = skip_comments(tokens, k + 1);
        // optional ASC / DESC
        if let Some(d) = tokens.get(k) {
            if is_word(d, "ASC") || is_word(d, "DESC") { k = skip_comments(tokens, k + 1); }
        }
        match tokens.get(k).map(|t| t.text) {
            Some(",") => k = skip_comments(tokens, k + 1),
            Some(")") => return Some((cols, k + 1)),
            _ => return None,
        }
    }
}

/// Every index the batch declares. Unparseable declarations are skipped, which
/// is the safe direction: we then behave exactly as before (recommend an index).
fn declared_indexes(tokens: &[Token<'_>]) -> Vec<DeclaredIndex> {
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "INDEX") { continue; }
        // Must be a CREATE ... INDEX, not DROP INDEX / ALTER INDEX / a column named INDEX.
        let mut back = i;
        let mut is_create = false;
        for _ in 0..4 {
            if back == 0 { break; }
            back -= 1;
            let p = &tokens[back];
            if p.kind == TokKind::Comment { continue; }
            if is_word(p, "CREATE") { is_create = true; break; }
            if is_word(p, "UNIQUE") || is_word(p, "CLUSTERED") || is_word(p, "NONCLUSTERED") { continue; }
            break;
        }
        if !is_create { continue; }

        // INDEX <name> ON <table_ref> (
        let mut k = skip_comments(tokens, i + 1);
        if tokens.get(k).map(|t| t.kind) != Some(TokKind::Word) { continue; }
        let index_name = bare(&tokens[k]).to_string();
        k = skip_comments(tokens, k + 1);
        if !tokens.get(k).is_some_and(|t| is_word(t, "ON")) { continue; }
        k = skip_comments(tokens, k + 1);

        // table reference: Word [. Word]*
        let mut table = String::new();
        while let Some(tok) = tokens.get(k) {
            if tok.kind == TokKind::Word { table.push_str(tok.text); }
            else if tok.text == "." { table.push('.'); }
            else { break; }
            k = skip_comments(tokens, k + 1);
        }
        if table.is_empty() { continue; }

        let Some((key_cols, after_key)) = column_list(tokens, k) else { continue };
        let mut include_cols = Vec::new();
        let j = skip_comments(tokens, after_key);
        if tokens.get(j).is_some_and(|t| is_word(t, "INCLUDE")) {
            let open = skip_comments(tokens, j + 1);
            if let Some((cols, _)) = column_list(tokens, open) { include_cols = cols; }
        }
        out.push(DeclaredIndex {
            name: index_name,
            table: table_short_name(&table),
            key_cols,
            include_cols,
        });
    }
    out
}

/// The bare table name (last dotted segment, brackets stripped) for naming.
fn table_short_name(table_ref: &str) -> String {
    let last = table_ref.rsplit('.').next().unwrap_or(table_ref);
    ident_frag(last.trim_matches(|c| c == '[' || c == ']'))
}

/// Build a `CREATE NONCLUSTERED INDEX` statement string. Key = eq cols then a
/// single range col; INCLUDE = projected cols not already in the key.
fn build_create_index(stmt: &SelectStmt, include_cols: &[String]) -> String {
    build_create_index_as(stmt, include_cols, None)
}

/// `redefine` names an index the batch already declares that this DDL should
/// replace. Emitting a second `CREATE INDEX` under a name already in use just
/// fails, so we reuse the name and add `DROP_EXISTING = ON` — the supported way
/// to redefine an index in place, keeping the copy-paste promise honest.
fn build_create_index_as(
    stmt: &SelectStmt,
    include_cols: &[String],
    redefine: Option<&str>,
) -> String {
    let mut key: Vec<String> = stmt.eq_cols.iter().map(|c| c.name.clone()).collect();
    // SARGable ordering: at most one trailing range column is useful as a key.
    if let Some(first_range) = stmt.range_cols.first() {
        key.push(first_range.name.clone());
    }

    let key_join = key
        .iter()
        .map(|c| format!("[{}]", ident_frag(c)))
        .collect::<Vec<_>>()
        .join(", ");

    // INCLUDE = projected cols minus anything already in the key.
    let incl: Vec<String> = include_cols
        .iter()
        .filter(|c| !key.iter().any(|k| name_eq_ci(k, c)))
        .cloned()
        .collect();

    let name = match redefine {
        Some(existing) => existing.to_string(),
        None => format!(
            "IX_{}_{}",
            table_short_name(&stmt.table_ref),
            key.iter().map(|c| ident_frag(c)).collect::<Vec<_>>().join("_")
        ),
    };

    let mut sql = format!(
        "CREATE NONCLUSTERED INDEX [{}]\n    ON {} ({})",
        name, stmt.table_ref, key_join
    );
    if !incl.is_empty() {
        let incl_join = incl
            .iter()
            .map(|c| format!("[{}]", ident_frag(c)))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!("\n    INCLUDE ({})", incl_join));
    }
    if redefine.is_some() {
        sql.push_str("\n    WITH (DROP_EXISTING = ON)");
    }
    sql.push(';');
    sql
}

// ===========================================================================
// RULE (a): missing index from WHERE equality / range predicates
// ===========================================================================

/// Single-table SELECT with WHERE equality / range predicates we can read →
/// emit a concrete CREATE NONCLUSTERED INDEX (equality keys first, one trailing
/// range key) with an INCLUDE list covering the projection.
pub fn missing_index_from_predicate(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let declared = declared_indexes(ctx.tokens);
    for stmt in parse_single_table_selects(ctx.tokens) {
        // Need at least one seekable predicate column to recommend a key.
        if stmt.eq_cols.is_empty() && stmt.range_cols.is_empty() { continue; }
        // The message asserts no matching index is declared in this batch, so
        // check before saying it. A declared index leading with the same column
        // already provides the seek; recommending a duplicate is noise.
        let leading = stmt
            .eq_cols
            .first()
            .or_else(|| stmt.range_cols.first())
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let table = table_short_name(&stmt.table_ref);
        if declared
            .iter()
            .any(|d| name_eq_ci(&d.table, &table) && d.leads_with(&leading))
        {
            continue;
        }
        // Confidence guard: at least one equality column, OR a single clean range
        // column. Pure multi-range with no equality is weaker; still emit but it
        // remains a valid (single trailing range) key.
        let anchor = stmt
            .eq_cols
            .first()
            .or_else(|| stmt.range_cols.first())
            .map(|c| c.tok)
            .unwrap_or(stmt.select_tok);

        // INCLUDE only when we confidently parsed the projection (not SELECT *).
        let include: Vec<String> = if stmt.select_star { Vec::new() } else { stmt.select_cols.clone() };

        let ddl = build_create_index(&stmt, &include);

        let key_desc = {
            let mut parts: Vec<String> = stmt.eq_cols.iter().map(|c| format!("{} (=)", c.name)).collect();
            if let Some(r) = stmt.range_cols.first() {
                parts.push(format!("{} (range)", r.name));
            }
            parts.join(", ")
        };

        let star_note = if stmt.select_star {
            "  (Projection is SELECT * — listing real columns would let the index cover the query via INCLUDE.)"
        } else {
            ""
        };

        out.push(finding(
            "index.missing_index_from_predicate",
            Severity::Info,
            format!(
                "Single-table SELECT on {} filters by {} but no matching index is declared in this batch. The optimizer may scan the whole table.",
                stmt.table_ref, key_desc
            ),
            Some(make_loc(&ctx.tokens[anchor])),
            Some(format!(
                "Add a covering nonclustered index (equality columns first, range column last):\n\n{}\n{}\nVerify against the actual plan / sys.dm_db_missing_index_details before deploying; an index has write-side cost.",
                ddl, star_note
            )),
        ));
    }
    out
}

// ===========================================================================
// RULE (b): ORDER BY that doesn't match the WHERE equality columns -> sort
// ===========================================================================

/// Single-table SELECT whose ORDER BY columns aren't covered by the WHERE
/// equality columns → the engine adds an explicit Sort. A covering index keyed
/// (equality cols, then ORDER BY cols) returns rows pre-sorted.
pub fn order_by_forces_sort(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for stmt in parse_single_table_selects(ctx.tokens) {
        if stmt.order_cols.is_empty() { continue; }

        // If the ORDER BY leading column is itself an equality-filtered column,
        // a single-value filter makes the sort trivial — skip (low value, risk
        // of noise). We fire only when ORDER BY adds genuinely new sort columns.
        let order_all_in_eq = stmt
            .order_cols
            .iter()
            .all(|o| stmt.eq_cols.iter().any(|e| name_eq_ci(&e.name, &o.name)));
        if order_all_in_eq { continue; }

        // We must be confident: only fire when there's at least one equality
        // predicate (so the suggested index has a sensible leading key) OR the
        // ORDER BY is the only access path (no predicate at all). Mixed range +
        // order is left to rule (a)/manual review to avoid a wrong key order.
        if !stmt.range_cols.is_empty() && stmt.eq_cols.is_empty() {
            // pure range + order: range and sort columns may conflict for key
            // order; don't guess.
            continue;
        }

        let anchor = stmt.order_cols[0].tok;

        // Build a sort-avoiding key: equality cols, then ORDER BY cols (dedup).
        let mut key: Vec<String> = stmt.eq_cols.iter().map(|c| c.name.clone()).collect();
        for o in &stmt.order_cols {
            if !key.iter().any(|k| name_eq_ci(k, &o.name)) {
                key.push(o.name.clone());
            }
        }
        let key_join = key
            .iter()
            .map(|c| format!("[{}]", ident_frag(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let include: Vec<String> = if stmt.select_star {
            Vec::new()
        } else {
            stmt.select_cols
                .iter()
                .filter(|c| !key.iter().any(|k| name_eq_ci(k, c)))
                .cloned()
                .collect()
        };
        let name = format!(
            "IX_{}_{}",
            table_short_name(&stmt.table_ref),
            key.iter().map(|c| ident_frag(c)).collect::<Vec<_>>().join("_")
        );
        let mut ddl = format!(
            "CREATE NONCLUSTERED INDEX [{}]\n    ON {} ({})",
            name, stmt.table_ref, key_join
        );
        if !include.is_empty() {
            let incl_join = include
                .iter()
                .map(|c| format!("[{}]", ident_frag(c)))
                .collect::<Vec<_>>()
                .join(", ");
            ddl.push_str(&format!("\n    INCLUDE ({})", incl_join));
        }
        ddl.push(';');

        let order_desc = stmt
            .order_cols
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
            .join(", ");

        out.push(finding(
            "index.order_by_forces_sort",
            Severity::Info,
            format!(
                "ORDER BY {} on {} isn't served by the filter columns, so the engine must add an explicit Sort. An index that keys the sort columns returns rows already ordered.",
                order_desc, stmt.table_ref
            ),
            Some(make_loc(&ctx.tokens[anchor])),
            Some(format!(
                "Create an index whose key ends with the ORDER BY columns so the Sort operator disappears:\n\n{}\nMatch the ASC/DESC direction of the ORDER BY in the index key if the sort is single-direction-critical.",
                ddl
            )),
        ));
    }
    out
}

// ===========================================================================
// RULE (c): key-lookup risk — several projected columns + narrow predicate
// ===========================================================================

/// Single-table SELECT projecting several explicitly-named columns behind a
/// narrow (equality) predicate → if only a non-covering index exists, each row
/// pays a key lookup. A covering index with INCLUDE removes the lookups. Fires
/// only when we confidently read both the predicate AND a real (non-*) column
/// list of meaningful width.
pub fn key_lookup_risk(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let declared = declared_indexes(ctx.tokens);
    for stmt in parse_single_table_selects(ctx.tokens) {
        // Require a narrow predicate: at least one equality column, no range
        // (range widens the seek and changes the calculus).
        if stmt.eq_cols.is_empty() || !stmt.range_cols.is_empty() { continue; }
        // Require a genuine, understood projection of several columns.
        if stmt.select_star || stmt.select_cols.is_empty() { continue; }
        // A key lookup presupposes a seek — which presupposes an index. When the
        // batch declares none, `missing_index_from_predicate` already emits the
        // covering DDL, and speaking here would duplicate it under the same
        // generated name. Only fire when the seek exists and fails to cover.
        let table = table_short_name(&stmt.table_ref);
        let seek = declared
            .iter()
            .find(|d| name_eq_ci(&d.table, &table) && d.leads_with(&stmt.eq_cols[0].name));
        let Some(seek) = seek else { continue };
        if seek.covers(&stmt.select_cols) { continue; }
        // Count projected columns that are NOT already the predicate keys —
        // those are exactly the columns a key lookup would have to fetch.
        let lookup_cols: Vec<String> = stmt
            .select_cols
            .iter()
            .filter(|c| !stmt.eq_cols.iter().any(|e| name_eq_ci(&e.name, c)))
            .cloned()
            .collect();
        // "Several" -> at least 3 fetched columns. Below that, a key lookup is
        // cheap and a covering index may not pay off; stay conservative.
        if lookup_cols.len() < 3 { continue; }

        let anchor = stmt.eq_cols[0].tok;
        let ddl = build_create_index_as(&stmt, &stmt.select_cols, Some(&seek.name));

        out.push(finding(
            "index.key_lookup_risk",
            Severity::Info,
            format!(
                "SELECT on {} returns {} columns behind a narrow equality filter, but [{}] keys {} without covering them. Every matching row pays a key lookup back to the base table.",
                stmt.table_ref, stmt.select_cols.len(), seek.name, stmt.eq_cols[0].name
            ),
            Some(make_loc(&ctx.tokens[anchor])),
            Some(format!(
                "Redefine that index so it covers the query — same key, INCLUDE the fetched columns:\n\n{}\nConfirm with the actual plan that a Key Lookup / RID Lookup is present before rebuilding; a wider INCLUDE costs more on write.",
                ddl
            )),
        ));
    }
    out
}

// ===========================================================================
// tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use crate::{analyze, AnalyzeInput};
    use crate::findings::Finding;

    fn run(sql: &str) -> Vec<Finding> {
        analyze(&AnalyzeInput {
            sql: Some(sql.to_string()),
            server_version: Some(2025),
            ..Default::default()
        })
        .findings
    }

    fn fired(sql: &str, id: &str) -> Option<Finding> {
        run(sql).into_iter().find(|f| f.rule.0 == id)
    }

    // ---- rule (a): missing_index_from_predicate -----------------------------

    #[test]
    fn missing_index_equality_fires_with_location_and_ddl() {
        let sql = "SELECT OrderId, CustomerId, Total FROM dbo.Orders WHERE CustomerId = @cid AND Status = 'Open';";
        let f = fired(sql, "index.missing_index_from_predicate")
            .expect("rule (a) should fire on single-table equality SELECT");
        assert!(f.location.is_some(), "must set a location");
        let rec = f.recommendation.unwrap();
        assert!(rec.contains("CREATE NONCLUSTERED INDEX"), "rec must contain runnable DDL: {rec}");
        assert!(rec.contains("dbo.Orders"), "DDL must name the real table: {rec}");
        // equality columns belong in the key
        assert!(rec.contains("[CustomerId]") && rec.contains("[Status]"), "key cols missing: {rec}");
    }

    #[test]
    fn missing_index_range_goes_last_in_key() {
        let sql = "SELECT Id FROM dbo.Events WHERE TenantId = 5 AND CreatedAt > '2026-01-01';";
        let f = fired(sql, "index.missing_index_from_predicate").expect("should fire");
        let rec = f.recommendation.unwrap();
        // TenantId (equality) must come before CreatedAt (range) in the ON(...) key.
        let on = rec.find("ON ").unwrap();
        let tenant = rec[on..].find("[TenantId]").unwrap();
        let created = rec[on..].find("[CreatedAt]").unwrap();
        assert!(tenant < created, "equality key must precede range key: {rec}");
    }

    #[test]
    fn missing_index_does_not_fire_on_join() {
        // Two tables -> we must NOT emit a single-table index recommendation.
        let sql = "SELECT o.Id FROM dbo.Orders o JOIN dbo.Customers c ON o.CustomerId = c.Id WHERE c.Status = 'X';";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire on a multi-table join");
    }

    #[test]
    fn missing_index_does_not_fire_on_or_predicate() {
        // OR can't be served by a single seek -> no recommendation.
        let sql = "SELECT Id FROM dbo.T WHERE A = 1 OR B = 2;";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire when WHERE has a top-level OR");
    }

    #[test]
    fn missing_index_does_not_fire_on_function_predicate() {
        // Function-wrapped column is non-SARGable; we don't propose a key on it.
        let sql = "SELECT Id FROM dbo.T WHERE UPPER(Name) = 'X';";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not propose an index key on a function-wrapped column");
    }

    #[test]
    fn missing_index_does_not_fire_on_subquery_from() {
        let sql = "SELECT Id FROM (SELECT Id FROM dbo.T) AS x WHERE x.Id = 1;";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire when FROM is a derived table");
    }

    // ---- FP regressions: parenthesized OR-group inside an AND chain ----------
    // A disjunction wrapped in parens is depth-1, so the old depth-0-only OR
    // bail missed it; the conjunct splitter then truncated `(B = 2 OR C = 3)`
    // to a bare `B = 2` and fabricated an index key on an OR-leg column. These
    // are correct, idiomatic T-SQL (kitchen-sink / optional-parameter filters)
    // and must NOT produce an index recommendation.

    #[test]
    fn missing_index_does_not_fire_on_parenthesized_or_in_and_chain() {
        let sql = "SELECT Id FROM dbo.T WHERE A = 1 AND (B = 2 OR C = 3) AND D = 4;";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire: B/C only appear inside a disjunction, not a sound seek key");
    }

    #[test]
    fn missing_index_does_not_fire_on_bare_parenthesized_or() {
        let sql = "SELECT Id FROM dbo.T WHERE (A = 1 OR B = 2);";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire on a single parenthesized OR group");
    }

    #[test]
    fn missing_index_does_not_fire_on_double_parenthesized_or() {
        let sql = "SELECT Id FROM dbo.T WHERE ((A = 1 OR B = 2));";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire on a doubly-parenthesized OR group");
    }

    #[test]
    fn missing_index_does_not_key_on_or_leg_with_leading_equality() {
        // The leading equality (Tenant) is sound, but the parenthesized OR must
        // still bail the whole WHERE so we don't emit IX_T_Tenant_A on an OR leg.
        let sql = "SELECT Id FROM dbo.T WHERE Tenant = 1 AND (A = 1 OR B = 2);";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire / must not key on an OR-leg column when an OR is present");
    }

    // ---- FP regression: optional-parameter idiom (@p IS NULL OR Col = @p) ----
    #[test]
    fn missing_index_does_not_fire_on_optional_parameter_pattern() {
        let sql = "SELECT Id FROM dbo.T WHERE (@p IS NULL OR Col = @p);";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not fire on the optional-parameter OR idiom");
    }

    // ---- FP regression: Col = NULL is the constant UNKNOWN, never a seek key --
    #[test]
    fn missing_index_does_not_fire_on_equals_null() {
        let sql = "SELECT Id FROM dbo.T WHERE Col = NULL;";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not recommend an index for `= NULL` (matches zero rows under ANSI_NULLS)");
    }

    // ---- FP regression: trailing-junk RHS must not be read as a clean key -----
    #[test]
    fn missing_index_does_not_fire_on_arithmetic_rhs() {
        // `col = 1 + 2` — the old code read `Col = 1` and dropped `+ 2`.
        let sql = "SELECT Id FROM dbo.T WHERE Col = 1 + 2;";
        assert!(fired(sql, "index.missing_index_from_predicate").is_none(),
            "must not truncate an arithmetic RHS to a bare equality key");
    }

    // ---- guard: the fixes must NOT kill genuine true positives ---------------
    #[test]
    fn missing_index_still_fires_on_clean_equality_after_fixes() {
        let sql = "SELECT Id FROM dbo.T WHERE A = 1 AND B = 2 AND D = 4;";
        let f = fired(sql, "index.missing_index_from_predicate")
            .expect("a pure-AND equality chain must still produce a recommendation");
        let rec = f.recommendation.unwrap();
        assert!(rec.contains("[A]") && rec.contains("[B]") && rec.contains("[D]"),
            "all three equality columns belong in the key: {rec}");
    }

    #[test]
    fn missing_index_still_fires_with_getdate_range_after_fixes() {
        // The end-of-conjunct check must still accept a function-valued RHS.
        let sql = "SELECT Id FROM dbo.T WHERE TenantId = 5 AND CreatedAt > GETDATE();";
        let f = fired(sql, "index.missing_index_from_predicate")
            .expect("GETDATE() is a legitimate runtime constant for a range predicate");
        let rec = f.recommendation.unwrap();
        assert!(rec.contains("[TenantId]") && rec.contains("[CreatedAt]"),
            "key must include both filter columns: {rec}");
    }

    #[test]
    fn missing_index_still_fires_with_dateadd_range_after_fixes() {
        let sql = "SELECT Id FROM dbo.T WHERE TenantId = 5 AND CreatedAt > DATEADD(day, -7, GETDATE());";
        assert!(fired(sql, "index.missing_index_from_predicate").is_some(),
            "DATEADD(...) with a balanced arg list is still a valid range constant");
    }

    // ---- rule (b): order_by_forces_sort -------------------------------------

    #[test]
    fn order_by_sort_fires() {
        let sql = "SELECT Id, Name FROM dbo.People WHERE TenantId = 1 ORDER BY LastName, FirstName;";
        let f = fired(sql, "index.order_by_forces_sort")
            .expect("rule (b) should fire when ORDER BY adds new sort columns");
        assert!(f.location.is_some());
        let rec = f.recommendation.unwrap();
        assert!(rec.contains("CREATE NONCLUSTERED INDEX"), "rec must include DDL: {rec}");
        assert!(rec.contains("[LastName]") && rec.contains("[FirstName]"), "sort cols must be in key: {rec}");
    }

    #[test]
    fn order_by_sort_does_not_fire_when_order_matches_filter() {
        // ORDER BY column is the equality-filtered column -> trivial sort, skip.
        let sql = "SELECT Id FROM dbo.People WHERE Status = 'X' ORDER BY Status;";
        assert!(fired(sql, "index.order_by_forces_sort").is_none(),
            "must not fire when ORDER BY is fully covered by equality filter");
    }

    #[test]
    fn order_by_sort_does_not_fire_on_join() {
        let sql = "SELECT a.Id FROM dbo.A a JOIN dbo.B b ON a.Id = b.Id ORDER BY a.Name;";
        assert!(fired(sql, "index.order_by_forces_sort").is_none(),
            "must not fire on a multi-table join");
    }

    // ---- rule (c): key_lookup_risk ------------------------------------------

    #[test]
    fn key_lookup_fires_when_declared_index_does_not_cover() {
        // A key lookup presupposes a seek, which presupposes an index. With one
        // declared but no INCLUDE, the four other columns each cost a lookup.
        let sql = "CREATE NONCLUSTERED INDEX IX_Customer_Code ON dbo.Customer (CustomerCode);\n\
                   SELECT Id, FirstName, LastName, Email, Phone FROM dbo.Customer WHERE CustomerCode = @c;";
        let f = fired(sql, "index.key_lookup_risk")
            .expect("should fire: many columns behind a seek that doesn't cover them");
        assert!(f.location.is_some());
        let rec = f.recommendation.unwrap();
        assert!(rec.contains("INCLUDE"), "covering index must use INCLUDE: {rec}");
        assert!(rec.contains("[CustomerCode]"), "filter col must be the key: {rec}");
        // It must redefine the index that already exists rather than emit a
        // second CREATE under a name the batch has already used.
        assert!(rec.contains("IX_Customer_Code"), "must reuse the declared name: {rec}");
        assert!(rec.contains("DROP_EXISTING = ON"), "must be runnable as written: {rec}");
    }

    #[test]
    fn key_lookup_defers_to_missing_index_when_nothing_is_declared() {
        // With no index in the batch, missing_index_from_predicate already emits
        // the covering DDL. Firing here too produced two CREATE statements under
        // the same generated name, the second of which fails.
        let sql = "SELECT Id, FirstName, LastName, Email, Phone FROM dbo.Customer WHERE CustomerCode = @c;";
        assert!(fired(sql, "index.key_lookup_risk").is_none(),
            "must defer to the missing-index rule when no index is declared");
        assert!(fired(sql, "index.missing_index_from_predicate").is_some(),
            "the missing-index rule must still cover this case");
    }

    #[test]
    fn key_lookup_does_not_fire_on_select_star() {
        // We can't build a trustworthy INCLUDE list from '*'.
        let sql = "SELECT * FROM dbo.Customer WHERE CustomerCode = @c;";
        assert!(fired(sql, "index.key_lookup_risk").is_none(),
            "must not fire on SELECT * (no real column list)");
    }

    #[test]
    fn key_lookup_does_not_fire_on_few_columns() {
        // Only 2 fetched columns besides the key -> below the "several" threshold.
        let sql = "SELECT Id, Name FROM dbo.Customer WHERE CustomerCode = @c;";
        assert!(fired(sql, "index.key_lookup_risk").is_none(),
            "must not fire when only a couple of columns are fetched");
    }

    #[test]
    fn key_lookup_does_not_fire_with_range_predicate() {
        // Range predicate widens the seek; this rule is for narrow equality only.
        let sql = "SELECT Id, A, B, C, D FROM dbo.T WHERE CreatedAt > '2026-01-01';";
        assert!(fired(sql, "index.key_lookup_risk").is_none(),
            "must not fire when the predicate is a range");
    }

    // ---- FP regression (same root cause as rule (a)): parenthesized OR -------
    // The OR truncation made eq_cols=[A,B] and a wide understood projection,
    // so key_lookup_risk emitted a covering index keyed on the OR-leg column B.
    // With the WHERE-parse now bailing on any OR, eq_cols is empty and the rule
    // correctly stays silent.
    #[test]
    fn key_lookup_does_not_fire_on_parenthesized_or() {
        let sql = "SELECT Id, A, B, C FROM dbo.T WHERE A = 1 AND (B = 2 OR C = 3);";
        assert!(fired(sql, "index.key_lookup_risk").is_none(),
            "must not build a covering index keyed on an OR-leg column");
    }
}
