// Join-shape rules.
//
// `join_filter_missing_index` performs *offline* missing-index inference for the
// single most common real-world shape that the DMV-driven advisor and the
// existing token rules deliberately skip: a clean two-table INNER equijoin with
// a sargable equality filter on one side, e.g.
//
//     SELECT a.x, b.y
//     FROM   dbo.A AS a
//     JOIN   dbo.B AS b ON a.k = b.fk
//     WHERE  b.filtercol = @x;
//
// The table carrying the equality filter is the "probed" table — the engine
// seeks into it once per outer row, so an index keyed on
// (equality-filter columns…, join key) with the projected columns INCLUDEd is
// the textbook fix. We emit a copy-paste CREATE NONCLUSTERED INDEX for that
// table.
//
// This is intentionally conservative. We only fire when the statement is a
// single, clean two-table INNER equijoin with a sargable equality filter, and
// we bail on anything we cannot reason about confidently: 3+ tables, OUTER /
// CROSS / APPLY, OR predicates, functions on the join/filter column,
// subqueries, CTEs, or SELECT *.

use super::{finding, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token, word_eq_ci};

/// Strip surrounding [] brackets from an identifier token for comparisons /
/// display.
fn bare<'a>(t: &'a Token<'a>) -> &'a str {
    t.text.trim_matches(|c| c == '[' || c == ']')
}

fn iw(t: &Token, kw: &str) -> bool {
    t.kind == TokKind::Word && word_eq_ci(bare(t), kw)
}

/// A real (non-comment) token at index `i`, or None.
fn code<'a>(tokens: &'a [Token<'a>], i: usize) -> Option<&'a Token<'a>> {
    tokens.get(i).filter(|t| t.kind != TokKind::Comment)
}

/// Index of the next non-comment token at or after `from`.
fn next_code(tokens: &[Token<'_>], from: usize) -> Option<usize> {
    (from..tokens.len()).find(|&k| tokens[k].kind != TokKind::Comment)
}

/// A qualified column reference `alias . column` starting at token `i`.
/// Returns (alias, column, index_after_column) when the shape matches.
fn qualified_col<'a>(tokens: &'a [Token<'a>], i: usize) -> Option<(&'a Token<'a>, &'a Token<'a>, usize)> {
    let a = code(tokens, i)?;
    if a.kind != TokKind::Word { return None; }
    let dot_i = next_code(tokens, i + 1)?;
    if tokens[dot_i].text != "." { return None; }
    let col_i = next_code(tokens, dot_i + 1)?;
    let c = &tokens[col_i];
    if c.kind != TokKind::Word { return None; }
    Some((a, c, col_i + 1))
}

/// True if `alias . name (` — i.e. the qualified reference is actually a function
/// call, which we treat as a function-on-column and bail on.
fn is_followed_by_call(tokens: &[Token<'_>], after_col: usize) -> bool {
    next_code(tokens, after_col)
        .map(|p| tokens[p].text == "(")
        .unwrap_or(false)
}

pub fn join_filter_missing_index(ctx: &RuleCtx) -> Vec<Finding> {
    let tokens = ctx.tokens;
    let mut out = Vec::new();

    // ---- Whole-statement guards (conservative) ----------------------------
    // Count top-level (non-comment) keywords. More than one SELECT, or any
    // WITH / UNION / subquery paren-SELECT means we are not looking at the
    // clean single two-table shape — bail entirely rather than misread it.
    let mut select_count = 0u32;
    let mut from_count = 0u32;
    let mut join_count = 0u32;
    let mut where_count = 0u32;
    let mut select_idx = None;
    let mut from_idx = None;
    let mut join_idx = None;
    let mut where_idx = None;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word { continue; }
        if iw(t, "WITH") { return out; } // CTE — out of scope
        if iw(t, "UNION") || iw(t, "INTERSECT") || iw(t, "EXCEPT") { return out; }
        if iw(t, "APPLY") { return out; } // CROSS/OUTER APPLY — out of scope
        if iw(t, "SELECT") { select_count += 1; if select_idx.is_none() { select_idx = Some(i); } }
        else if iw(t, "FROM") { from_count += 1; if from_idx.is_none() { from_idx = Some(i); } }
        else if iw(t, "JOIN") { join_count += 1; if join_idx.is_none() { join_idx = Some(i); } }
        else if iw(t, "WHERE") { where_count += 1; if where_idx.is_none() { where_idx = Some(i); } }
    }
    // Exactly one SELECT, one FROM, one JOIN, one WHERE — a single clean
    // two-table query. Anything else (subquery, second statement, 3+ tables
    // via a second JOIN) takes us out of the supported shape.
    if select_count != 1 || from_count != 1 || join_count != 1 || where_count != 1 {
        return out;
    }
    let (select_i, from_i, join_i, where_i) =
        match (select_idx, from_idx, join_idx, where_idx) {
            (Some(s), Some(f), Some(j), Some(w)) if s < f && f < j && j < w => (s, f, j, w),
            _ => return out,
        };

    // ---- JOIN flavor guard: only bare INNER JOIN ---------------------------
    // The keyword immediately before JOIN tells us the flavor. Reject
    // LEFT/RIGHT/FULL/OUTER/CROSS, and the physical-algorithm hints
    // LOOP/HASH/MERGE/REMOTE (e.g. `INNER LOOP JOIN`).
    if let Some(prev) = (0..join_i).rev().find(|&k| tokens[k].kind != TokKind::Comment).map(|k| &tokens[k]) {
        let banned = ["LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "LOOP", "HASH", "MERGE", "REMOTE"];
        if banned.iter().any(|b| iw(prev, b)) {
            return out;
        }
    }
    // (a bare `INNER` before JOIN is fine; INNER is the default.)

    // ---- SELECT-list guard: no SELECT * ------------------------------------
    // Scan the projection between SELECT and FROM for a bare `*`.
    for k in (select_i + 1)..from_i {
        if tokens[k].kind == TokKind::Comment { continue; }
        if tokens[k].text == "*" { return out; }
    }

    // ---- Resolve table aliases for the two tables --------------------------
    // FROM <schema?.>table [AS] alias   and   JOIN <schema?.>table [AS] alias
    let from_tab = parse_table_ref(tokens, from_i + 1, join_i);
    let join_tab = parse_table_ref(tokens, join_i + 1, find_on(tokens, join_i + 1, where_i).unwrap_or(where_i));
    let (Some(from_tab), Some(join_tab)) = (from_tab, join_tab) else { return out; };

    // A table variable (@t) or temp table (#t) is not something we can index
    // with CREATE NONCLUSTERED INDEX ON a base table — bail.
    if from_tab.name.starts_with('@') || from_tab.name.starts_with('#')
        || join_tab.name.starts_with('@') || join_tab.name.starts_with('#') {
        return out;
    }

    // ---- ON clause: must be a single equijoin alias.col = alias.col --------
    let on_i = match find_on(tokens, join_i + 1, where_i) { Some(o) => o, None => return out };
    // Bounds of the ON predicate = (on_i, where_i).
    let onj = match parse_single_equijoin(tokens, on_i + 1, where_i, &from_tab, &join_tab) {
        Some(j) => j,
        None => return out,
    };

    // ---- WHERE clause: find a sargable equality filter ---------------------
    // We scan until end of the statement / clause boundary.
    let where_end = (where_i + 1..tokens.len())
        .find(|&k| {
            let t = &tokens[k];
            t.text == ";"
                || iw(t, "GROUP") || iw(t, "ORDER") || iw(t, "HAVING")
                || iw(t, "OPTION") || iw(t, "UNION")
        })
        .unwrap_or(tokens.len());

    // Bail on OR anywhere in the WHERE — a disjunction defeats the simple
    // single-seek story this rule is built on.
    for k in (where_i + 1)..where_end {
        if iw(&tokens[k], "OR") { return out; }
    }

    // Collect the equality-filter columns, grouped by which alias they target.
    // Only `alias.col = <literal|param>` counts (sargable equality, column bare
    // on its own side). Bail if a function wraps the column.
    let mut filt_from: Vec<String> = Vec::new();
    let mut filt_join: Vec<String> = Vec::new();
    let mut k = where_i + 1;
    while k < where_end {
        if tokens[k].kind == TokKind::Comment { k += 1; continue; }
        // A function call on a column inside WHERE → non-sargable shape, bail.
        if tokens[k].kind == TokKind::Word
            && next_code(tokens, k + 1).map(|p| tokens[p].text == "(").unwrap_or(false)
            && !word_eq_ci(bare(&tokens[k]), "AND")
        {
            // e.g. UPPER(b.col) — defeat the seek; out of scope.
            return out;
        }
        if let Some((alias, col, after)) = qualified_col(tokens, k) {
            if is_followed_by_call(tokens, after) {
                // alias.fn( … ) → scalar UDF / function on the column: bail.
                return out;
            }
            // equality operator?
            if let Some(op_i) = next_code(tokens, after) {
                if tokens[op_i].text == "=" {
                    // RHS must be a literal or @param or another simple operand,
                    // NOT a column on this same table (those are join-ish).
                    if let Some(rhs_i) = next_code(tokens, op_i + 1) {
                        let rhs = &tokens[rhs_i];
                        let rhs_is_value = rhs.kind == TokKind::String
                            || rhs.kind == TokKind::Number
                            || (rhs.kind == TokKind::Word && rhs.text.starts_with('@'))
                            || iw(rhs, "N"); // N'…' unicode literal prefix
                        if rhs_is_value {
                            let a = bare(alias);
                            let c = bare(col).to_string();
                            if from_tab.alias_matches(a) { push_unique(&mut filt_from, c); }
                            else if join_tab.alias_matches(a) { push_unique(&mut filt_join, c); }
                        }
                    }
                }
            }
            k = after;
            continue;
        }
        k += 1;
    }

    // Exactly one side must carry the equality filter — that side is the
    // "probed" table we recommend an index for. If both or neither carry an
    // equality filter, the shape is ambiguous → bail (conservative).
    let (probed, probe_join_col, filter_cols, projected) = if !filt_from.is_empty() && filt_join.is_empty() {
        (&from_tab, &onj.from_col, filt_from, collect_projection(tokens, select_i + 1, from_i, &from_tab))
    } else if filt_join.is_empty() == false && filt_from.is_empty() {
        (&join_tab, &onj.join_col, filt_join, collect_projection(tokens, select_i + 1, from_i, &join_tab))
    } else {
        return out;
    };

    // ---- Build the copy-paste CREATE NONCLUSTERED INDEX --------------------
    // Key = equality filter columns first (most selective seek), then the join
    // key of the probed table. INCLUDE = that table's projected columns that
    // are not already key columns.
    let mut key_cols: Vec<String> = Vec::new();
    for c in &filter_cols { push_unique(&mut key_cols, c.clone()); }
    push_unique(&mut key_cols, probe_join_col.clone());

    let include_cols: Vec<String> = projected
        .into_iter()
        .filter(|c| !key_cols.iter().any(|k| word_eq_ci(k, c)))
        .collect();

    let key_ddl = key_cols.iter().map(|c| format!("[{c}]")).collect::<Vec<_>>().join(", ");
    let include_ddl = if include_cols.is_empty() {
        String::new()
    } else {
        let cols = include_cols.iter().map(|c| format!("[{c}]")).collect::<Vec<_>>().join(", ");
        format!("\n    INCLUDE ({cols})")
    };
    let idx_name = format!(
        "IX_{}_{}",
        sanitize(&probed.name),
        key_cols.iter().map(|c| sanitize(c)).collect::<Vec<_>>().join("_"),
    );
    let table_ddl = probed.qualified();
    let ddl = format!(
        "CREATE NONCLUSTERED INDEX [{idx_name}]\n    ON {table_ddl} ({key_ddl}){include_ddl};"
    );

    out.push(finding(
        "index.join_filter_missing_index",
        Severity::Info,
        format!(
            "Two-table INNER equijoin filters {} on an equality predicate while joining on its key — the engine probes {} once per matching row. A covering index on the filter and join columns turns that probe into a seek.",
            probed.display(), probed.display(),
        ),
        Some(make_loc(&tokens[join_i])),
        Some(format!(
            "Consider a covering index on the probed table (verify column selectivity and consolidate with any existing index before applying):\n  {ddl}\nKey order puts the equality-filter column(s) first, then the join key. INCLUDE carries the projected columns so the query is covered. This is a static, shape-based suggestion — confirm with the actual execution plan and your real data distribution."
        )),
    ));

    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.iter().any(|x| word_eq_ci(x, &s)) { v.push(s); }
}

/// Replace anything that is not alphanumeric/underscore with `_` for use in an
/// index name.
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

#[derive(Debug)]
struct TableRef {
    /// schema-qualified name without alias, e.g. `dbo.Orders` (brackets stripped).
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

impl TableRef {
    fn alias_matches(&self, a: &str) -> bool {
        if let Some(al) = &self.alias {
            if word_eq_ci(al, a) { return true; }
        }
        // Also allow the bare table name as a qualifier when there is no alias.
        word_eq_ci(&self.name, a)
    }
    /// `[dbo].[Orders]` or `[Orders]`.
    fn qualified(&self) -> String {
        match &self.schema {
            Some(s) => format!("[{s}].[{}]", self.name),
            None => format!("[{}]", self.name),
        }
    }
    /// Human label for messages: alias if present else table name.
    fn display(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.name.clone())
    }
}

/// Parse a `[schema.]table [AS] alias` reference starting at `start`, not past
/// `end`. Returns None if it does not look like a simple base-table reference
/// (e.g. it opens a paren — a derived table / subquery).
fn parse_table_ref(tokens: &[Token<'_>], start: usize, end: usize) -> Option<TableRef> {
    let i = next_code_bounded(tokens, start, end)?;
    let first = &tokens[i];
    if first.text == "(" { return None; } // derived table / subquery
    if first.kind != TokKind::Word { return None; }

    // Optional schema.table
    let (schema, name, mut after) = {
        let dot = next_code_bounded(tokens, i + 1, end);
        if let Some(di) = dot {
            if tokens[di].text == "." {
                if let Some(ni) = next_code_bounded(tokens, di + 1, end) {
                    if tokens[ni].kind == TokKind::Word {
                        (Some(bare(first).to_string()), bare(&tokens[ni]).to_string(), ni + 1)
                    } else { return None; }
                } else { return None; }
            } else {
                (None, bare(first).to_string(), i + 1)
            }
        } else {
            (None, bare(first).to_string(), i + 1)
        }
    };

    // Optional alias: [AS] <word>. Skip an `AS` keyword if present.
    let mut alias = None;
    if let Some(ai) = next_code_bounded(tokens, after, end) {
        let mut cur = ai;
        if iw(&tokens[cur], "AS") {
            cur = match next_code_bounded(tokens, cur + 1, end) { Some(x) => x, None => after };
        }
        if cur < end && tokens[cur].kind == TokKind::Word && !is_clause_kw(&tokens[cur]) {
            alias = Some(bare(&tokens[cur]).to_string());
            after = cur + 1;
        }
    }
    let _ = after;
    Some(TableRef { schema, name, alias })
}

fn is_clause_kw(t: &Token) -> bool {
    ["JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "ON", "WHERE",
     "GROUP", "ORDER", "HAVING", "WITH", "OPTION", "UNION"]
        .iter()
        .any(|k| iw(t, k))
}

fn next_code_bounded(tokens: &[Token<'_>], from: usize, end: usize) -> Option<usize> {
    (from..end.min(tokens.len())).find(|&k| tokens[k].kind != TokKind::Comment)
}

/// Index of the ON keyword for the join, between `start` and `end`.
fn find_on(tokens: &[Token<'_>], start: usize, end: usize) -> Option<usize> {
    (start..end.min(tokens.len())).find(|&k| iw(&tokens[k], "ON"))
}

struct EquiJoin {
    from_col: String,
    join_col: String,
}

/// Parse the ON predicate (between `start` and `end`) and require it to be a
/// single equality `x.col = y.col` linking the two tables. Returns the column
/// belonging to each table. Bails (None) on functions, AND/OR, or multi-predicate.
fn parse_single_equijoin(
    tokens: &[Token<'_>],
    start: usize,
    end: usize,
    from_tab: &TableRef,
    join_tab: &TableRef,
) -> Option<EquiJoin> {
    // Reject any boolean connective — we only support a single equijoin.
    for k in start..end.min(tokens.len()) {
        if iw(&tokens[k], "AND") || iw(&tokens[k], "OR") { return None; }
    }
    // Expect: qualcol = qualcol
    let (a1, c1, after1) = qualified_col(tokens, next_code_bounded(tokens, start, end)?)?;
    if is_followed_by_call(tokens, after1) { return None; }
    let eq = next_code_bounded(tokens, after1, end)?;
    if tokens[eq].text != "=" { return None; }
    let (a2, c2, after2) = qualified_col(tokens, next_code_bounded(tokens, eq + 1, end)?)?;
    if is_followed_by_call(tokens, after2) { return None; }
    // Nothing meaningful should follow inside the ON clause.
    if next_code_bounded(tokens, after2, end).is_some() { return None; }

    let a1 = bare(a1);
    let a2 = bare(a2);
    // The two sides must reference the two different tables.
    let map = |a: &str| -> Option<bool> {
        // returns Some(true) if from-table, Some(false) if join-table
        if from_tab.alias_matches(a) { Some(true) }
        else if join_tab.alias_matches(a) { Some(false) }
        else { None }
    };
    let s1 = map(a1)?;
    let s2 = map(a2)?;
    if s1 == s2 { return None; } // both sides same table — not a join predicate
    let (from_col, join_col) = if s1 {
        (bare(c1).to_string(), bare(c2).to_string())
    } else {
        (bare(c2).to_string(), bare(c1).to_string())
    };
    Some(EquiJoin { from_col, join_col })
}

/// Collect the projected columns (from the SELECT list, between `start` and
/// `from_i`) that belong to `tab`, by alias qualification. Only qualified
/// `alias.col` references are collected — bare columns are ambiguous in a join
/// and are skipped (conservative; they simply won't be added to INCLUDE).
fn collect_projection(tokens: &[Token<'_>], start: usize, from_i: usize, tab: &TableRef) -> Vec<String> {
    let mut cols = Vec::new();
    let mut k = start;
    while k < from_i {
        if tokens[k].kind == TokKind::Comment { k += 1; continue; }
        if let Some((alias, col, after)) = qualified_col(tokens, k) {
            // Skip a function call / aggregate over the column.
            if !is_followed_by_call(tokens, after) && tab.alias_matches(bare(alias)) {
                push_unique(&mut cols, bare(col).to_string());
            }
            k = after;
            continue;
        }
        k += 1;
    }
    cols
}
