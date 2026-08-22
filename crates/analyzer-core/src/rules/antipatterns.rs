//! Query-shape antipatterns: idioms that are *syntactically* valid but usually
//! signal a more efficient / more correct rewrite.
//!
//! Every rule here is heavily false-positive-sensitive because the shapes it
//! looks for (UNION, COUNT(*), DISTINCT, scalar subqueries) all have entirely
//! legitimate uses. The guards below are deliberately conservative: when in
//! doubt we stay silent rather than nag on correct, idiomatic T-SQL.

use super::{finding, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token, word_eq_ci};

/// Stricter keyword test than the shared `super::is_word`.
///
/// The shared helper *strips* surrounding `[]` before comparing, which means a
/// delimited identifier deliberately named after a reserved word — e.g. the
/// column `[UNION]` or `[COUNT]` — matches the keyword check and produces a
/// false positive. A bracket- or double-quote-delimited token is, by
/// definition, an *identifier*, never a keyword, so we reject it outright.
fn kw(t: &Token, keyword: &str) -> bool {
    if t.kind != TokKind::Word {
        return false;
    }
    // Delimited identifiers can never be keywords.
    if t.text.starts_with('[') || t.text.starts_with('"') {
        return false;
    }
    // Variables / temp tables (`@x`, `#t`) are not keywords either.
    if t.text.starts_with('@') || t.text.starts_with('#') {
        return false;
    }
    word_eq_ci(t.text, keyword)
}

/// Strip surrounding `[]`/`"` from a (possibly delimited) identifier token.
fn ident_text<'a>(t: &Token<'a>) -> &'a str {
    t.text
        .trim_matches(|c| c == '[' || c == ']' || c == '"')
}

/// Index of the next non-comment token at or after `i`.
fn skip_comments(tokens: &[Token], mut i: usize) -> usize {
    while i < tokens.len() && tokens[i].kind == TokKind::Comment {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// UNION should (usually) be UNION ALL
// ---------------------------------------------------------------------------

/// `UNION` (without `ALL`) forces an implicit DISTINCT/sort to dedupe the
/// combined result. When the inputs are already disjoint (very common) that is
/// pure wasted work and `UNION ALL` is both faster and clearer.
///
/// Guards:
/// * `[UNION]` / `"UNION"` delimited identifiers are *not* the keyword (the
///   shared `is_word` would wrongly match them — see [`kw`]).
/// * `UNION ALL` is already correct and never fires.
/// Bounds of the SELECT branch immediately before the UNION at `union_idx`:
/// the index of its SELECT and whether it has a depth-0 FROM. Stops at the
/// enclosing `(` or a statement boundary.
fn branch_before(tokens: &[Token], union_idx: usize) -> Option<(usize, bool)> {
    let mut k = union_idx;
    let mut depth = 0i32;
    let mut has_from = false;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" {
            depth += 1;
            continue;
        }
        if t.text == "(" {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            if kw(t, "FROM") {
                has_from = true;
            }
            if kw(t, "SELECT") {
                return Some((k, has_from));
            }
            if t.text == ";" || kw(t, "UNION") {
                return None;
            }
        }
    }
    None
}

/// Does the SELECT branch after the UNION at `union_idx` have a depth-0 FROM?
fn branch_after_has_from(tokens: &[Token], union_idx: usize) -> bool {
    let mut j = union_idx + 1;
    let mut depth = 0i32;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            if depth == 0 {
                return false;
            }
            depth -= 1;
        } else if depth == 0 {
            if kw(t, "FROM") {
                return true;
            }
            if t.text == ";" || kw(t, "UNION") || kw(t, "ORDER") {
                return false;
            }
        }
        j += 1;
    }
    false
}

/// Both branches around this UNION are FROM-less scalar selects.
fn union_branches_are_scalar(tokens: &[Token], union_idx: usize) -> bool {
    match branch_before(tokens, union_idx) {
        Some((_, has_from)) => !has_from && !branch_after_has_from(tokens, union_idx),
        None => false,
    }
}

/// Normalised (select-list, from-source) of the SELECT at `select_idx`,
/// ignoring `TOP (…)`/`DISTINCT` and comments. `None` when the branch has no
/// depth-0 FROM.
fn branch_signature(tokens: &[Token], select_idx: usize) -> Option<(String, String)> {
    let mut j = select_idx + 1;
    let mut depth = 0i32;
    let mut list: Vec<String> = Vec::new();
    let mut src: Vec<String> = Vec::new();
    let mut in_from = false;
    let mut skip_top = false;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.kind == TokKind::Comment { j += 1; continue; }
        if t.text == "(" { depth += 1; }
        else if t.text == ")" {
            depth -= 1;
            if depth < 0 { break; }
        }
        if depth == 0 {
            if !in_from && (t.text == ";" || kw(t, "UNION") || kw(t, "ORDER")) { return None; }
            if in_from && (t.text == ";" || kw(t, "UNION") || kw(t, "WHERE") || kw(t, "ORDER")
                || kw(t, "GROUP") || kw(t, "HAVING") || kw(t, "OPTION"))
            {
                break;
            }
            if !in_from && kw(t, "FROM") { in_from = true; j += 1; continue; }
        }
        if !in_from {
            // `TOP (@n)` / `TOP 10` / `DISTINCT` are not part of the projection.
            if depth == 0 && kw(t, "TOP") { skip_top = true; j += 1; continue; }
            if skip_top {
                // consume `(…)` or a single number/variable
                if t.text == "(" { j += 1; continue; }
                if t.text == ")" && depth == 0 { skip_top = false; j += 1; continue; }
                if depth == 0 { skip_top = false; j += 1; continue; }
                j += 1; continue;
            }
            if depth == 0 && kw(t, "DISTINCT") { j += 1; continue; }
            list.push(t.text.to_ascii_lowercase());
        } else {
            src.push(t.text.to_ascii_lowercase());
        }
        j += 1;
    }
    if !in_from || src.is_empty() { return None; }
    Some((list.join(" "), src.join(" ")))
}

/// Are the two branches around this UNION the same projection over the same
/// FROM source? Then they can overlap and the implicit DISTINCT is intended.
fn union_branches_same_source_and_projection(tokens: &[Token], union_idx: usize) -> bool {
    let Some((before_sel, _)) = branch_before(tokens, union_idx) else { return false };
    let mut n = skip_comments(tokens, union_idx + 1);
    while tokens.get(n).map(|t| t.text == "(").unwrap_or(false) { n = skip_comments(tokens, n + 1); }
    if !tokens.get(n).map(|t| kw(t, "SELECT")).unwrap_or(false) { return false; }
    match (branch_signature(tokens, before_sel), branch_signature(tokens, n)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Is this UNION inside a derived table `FROM (…) x` whose consuming SELECT
/// list is a COUNT(...) / DISTINCT aggregate?
fn union_feeds_distinct_aggregate(tokens: &[Token], union_idx: usize) -> bool {
    // Enclosing `(` at depth 0.
    let mut k = union_idx;
    let mut depth = 0i32;
    let mut open: Option<usize> = None;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" {
            depth += 1;
        } else if t.text == "(" {
            if depth == 0 {
                open = Some(k);
                break;
            }
            depth -= 1;
        } else if depth == 0 && t.text == ";" {
            return false;
        }
    }
    let Some(open) = open else { return false };
    let Some(from_at) = prev_sig(tokens, open) else { return false };
    if !kw(&tokens[from_at], "FROM") {
        return false;
    }
    // Walk back from FROM to its SELECT, checking the list for COUNT/DISTINCT.
    let mut k = from_at;
    let mut depth = 0i32;
    let mut saw_agg = false;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" {
            depth += 1;
            continue;
        }
        if t.text == "(" {
            if depth == 0 {
                return false;
            }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            if kw(t, "COUNT") || kw(t, "COUNT_BIG") || kw(t, "DISTINCT") {
                saw_agg = true;
            }
            if kw(t, "SELECT") {
                return saw_agg;
            }
            if t.text == ";" {
                return false;
            }
        }
    }
    false
}

pub fn union_should_be_union_all(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !kw(t, "UNION") {
            continue;
        }
        // Next non-comment token: if it's ALL, this is already UNION ALL.
        let n = skip_comments(tokens, i + 1);
        if tokens.get(n).map(|x| kw(x, "ALL")).unwrap_or(false) {
            continue;
        }
        // `SELECT @a UNION SELECT @b UNION SELECT @c` — FROM-less scalar
        // branches exist to build a distinct value list; the dedupe IS the
        // point. Likewise a UNION derived table consumed by COUNT(...) /
        // DISTINCT is counting distinct values, and UNION ALL would change
        // the answer.
        if union_branches_are_scalar(tokens, i) || union_feeds_distinct_aggregate(tokens, i) {
            continue;
        }
        // `SELECT TOP (n) k FROM t ORDER BY a UNION SELECT TOP (n) k FROM t
        // ORDER BY b`: same source, same projection, no literal to tell the
        // branches apart — the rows are guaranteed to overlap and the dedupe
        // is the point. (Differing literals — `'Customers'` vs `'Suppliers'`
        // — make the branches provably disjoint and keep the finding.)
        if union_branches_same_source_and_projection(tokens, i) {
            continue;
        }
        // Three branches on one line are one piece of advice, not two.
        if out.iter().any(|f: &Finding| f.location.as_ref().map(|l| l.line) == Some(t.line)) {
            continue;
        }
        out.push(finding(
            "antipattern.union_should_be_union_all",
            Severity::Info,
            "UNION (without ALL) performs an implicit DISTINCT — an extra sort/hash to remove duplicate rows across the inputs. When the branches are already disjoint this is wasted work.",
            Some(make_loc(t)),
            Some("If the combined inputs cannot produce duplicate rows, use UNION ALL — it skips the dedupe pass and is both faster and clearer. Reserve plain UNION for when you genuinely need cross-branch de-duplication.".into()),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// COUNT(*) used as an existence test
// ---------------------------------------------------------------------------

/// `IF (SELECT COUNT(*) …) > 0` (and equivalents) makes the engine count *every*
/// matching row just to answer a yes/no question. `EXISTS` short-circuits on the
/// first match.
///
/// Crucially we ONLY fire on operator/literal combinations that collapse to a
/// boolean existence boundary:
///   * "at least one":  `> 0`, `>= 1`, `<> 0`, `!= 0`
///   * "none":          `= 0`, `< 1`, `<= 0`
/// We deliberately do NOT fire on `= 1` (nor `<= 1`, `< 2`), which assert a
/// specific *cardinality* (exactly/at-most one). `EXISTS` cannot express those,
/// so the rewrite would silently change behavior — a real false positive.
pub fn count_for_existence(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        // Look for COUNT ( * )
        if !kw(t, "COUNT") {
            continue;
        }
        let lp = skip_comments(tokens, i + 1);
        if tokens.get(lp).map(|x| x.text == "(").unwrap_or(false) == false {
            continue;
        }
        let star = skip_comments(tokens, lp + 1);
        if tokens.get(star).map(|x| x.text == "*").unwrap_or(false) == false {
            continue;
        }
        let rp = skip_comments(tokens, star + 1);
        if tokens.get(rp).map(|x| x.text == ")").unwrap_or(false) == false {
            continue;
        }
        // `GROUP BY … HAVING COUNT(*) > 0` is a per-group predicate, never an
        // existence test — every group has at least one row, and EXISTS has
        // no HAVING form to rewrite to.
        if count_is_in_having(tokens, i) {
            continue;
        }

        // The comparison applies to the COUNT scalar, which is frequently
        // wrapped in a scalar subquery: `(SELECT COUNT(*) FROM … WHERE …) > 0`.
        // Walk forward from COUNT's `)` to the position just after the wrapping
        // subquery closes (paren depth returns below the COUNT level), then read
        // the comparison operator + literal. If COUNT(*) is bare (`COUNT(*) > 0`)
        // the operator sits immediately after `rp`, depth never dips, so we also
        // try `rp + 1` directly.
        let after = forward_compare_pos(tokens, rp);
        if let Some((op, lit)) = read_op_then_number(tokens, after) {
            if is_existence_boundary(&op, &lit) {
                // `CASE WHEN (SELECT COUNT(*) …) > 0 THEN (SELECT COUNT(*) …)`
                // returns the count itself, so it has to be computed anyway.
                if count_is_also_returned(tokens, i, after) {
                    continue;
                }
                push_count_existence(&mut out, t);
                continue;
            }
        }
        // Reverse form: `<number> <op> COUNT(*)`. Walk back from COUNT over the
        // operator run to a leading number literal.
        if let Some((op, lit)) = read_number_then_op_before(tokens, i) {
            // Flip operator orientation: `0 < COUNT(*)` ≡ `COUNT(*) > 0`.
            let flipped = flip_op(&op);
            if is_existence_boundary(&flipped, &lit) {
                push_count_existence(&mut out, t);
                continue;
            }
        }
    }
    out
}

/// Is the COUNT at `count_idx` inside a HAVING clause of the same query
/// (walking back at paren depth 0, before reaching the owning SELECT)?
fn count_is_in_having(tokens: &[Token], count_idx: usize) -> bool {
    let mut k = count_idx;
    let mut depth = 0i32;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" {
            depth += 1;
            continue;
        }
        if t.text == "(" {
            if depth == 0 {
                return false;
            }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            if kw(t, "HAVING") {
                return true;
            }
            if kw(t, "SELECT") || kw(t, "WHERE") || t.text == ";" {
                return false;
            }
        }
    }
    false
}

/// For the wrapped form `(SELECT COUNT(*) …) <op> <n>` read at `cmp_at`: is the
/// very same subquery returned as a value right after (`THEN (SELECT COUNT(*) …)`)?
fn count_is_also_returned(tokens: &[Token], count_idx: usize, cmp_at: usize) -> bool {
    // The wrapping subquery: walk back from COUNT to its `(`.
    let Some(sel) = prev_sig(tokens, count_idx) else { return false };
    if !kw(&tokens[sel], "SELECT") {
        return false;
    }
    let Some(open) = prev_sig(tokens, sel) else { return false };
    if tokens[open].text != "(" {
        return false;
    }
    let Some(close) = matching_paren(tokens, open) else { return false };
    let sub: Vec<String> = tokens[open..=close]
        .iter()
        .filter(|t| t.kind != TokKind::Comment)
        .map(|t| t.text.to_ascii_lowercase())
        .collect();
    // Past the operator and literal: expect THEN, then the same token run.
    let mut j = cmp_at;
    while j < tokens.len() && tokens[j].kind == TokKind::Punct && matches!(tokens[j].text, ">" | "<" | "=" | "!") {
        j += 1;
    }
    j = skip_comments(tokens, j);
    if tokens.get(j).map(|t| t.kind != TokKind::Number).unwrap_or(true) {
        return false;
    }
    j = skip_comments(tokens, j + 1);
    if !tokens.get(j).map(|t| kw(t, "THEN")).unwrap_or(false) {
        return false;
    }
    j = skip_comments(tokens, j + 1);
    let mut m = 0usize;
    while m < sub.len() {
        let Some(t) = tokens.get(j) else { return false };
        if t.kind == TokKind::Comment {
            j += 1;
            continue;
        }
        if t.text.to_ascii_lowercase() != sub[m] {
            return false;
        }
        m += 1;
        j += 1;
    }
    true
}

fn push_count_existence(out: &mut Vec<Finding>, t: &Token) {
    out.push(finding(
        "antipattern.count_for_existence",
        Severity::Info,
        "COUNT(*) compared to an existence boundary forces the engine to count every matching row just to answer a yes/no question.",
        Some(make_loc(t)),
        Some("Use IF EXISTS (SELECT 1 FROM …) / WHERE EXISTS (…). EXISTS short-circuits on the first matching row instead of scanning the whole set to build a count.".into()),
    ));
}

/// Given the index of the `)` that closes `COUNT(*)`, return the token index at
/// which the existence comparison should be read.
///
/// * Bare `COUNT(*) > 0` — the operator sits at `count_rp + 1`.
/// * Wrapped `(SELECT COUNT(*) FROM … WHERE …) > 0` — we step over the rest of
///   the enclosing scalar subquery to the token just past its closing `)`.
///
/// We scan forward tracking paren depth relative to `count_rp`. The first time
/// depth drops below 0 (an unmatched `)` closing a paren opened *before* COUNT)
/// the wrapping subquery has closed; the comparison follows it. If we hit a
/// statement boundary or a comparison operator first, return that position.
fn forward_compare_pos(tokens: &[Token], count_rp: usize) -> usize {
    // Bare form: the operator is immediately after COUNT(*)'s `)`.
    let next = skip_comments(tokens, count_rp + 1);
    if tokens
        .get(next)
        .map(|tk| tk.kind == TokKind::Punct && matches!(tk.text, ">" | "<" | "=" | "!"))
        .unwrap_or(false)
    {
        return next;
    }

    // Wrapped form: step over the rest of the enclosing scalar subquery to the
    // token just past its closing `)`. Depth starts at 0; the first unmatched
    // `)` (depth would go negative) closes the wrapping subquery. Operators seen
    // before that belong to the subquery's own WHERE and must be ignored.
    let mut j = count_rp + 1;
    let mut depth = 0i32;
    while j < tokens.len() {
        match tokens[j].text {
            "(" => depth += 1,
            ")" => {
                if depth == 0 {
                    return skip_comments(tokens, j + 1);
                }
                depth -= 1;
            }
            ";" if depth == 0 => return j,
            _ => {}
        }
        j += 1;
    }
    count_rp + 1
}

/// Read a comparison operator run (`>`, `>=`, `<>`, `!=`, `=`, `<`, `<=`)
/// starting at `start`, followed by a numeric literal. Returns the normalized
/// operator string and the literal text.
fn read_op_then_number(tokens: &[Token], start: usize) -> Option<(String, String)> {
    let mut i = skip_comments(tokens, start);
    let mut op = String::new();
    let mut steps = 0;
    while let Some(tk) = tokens.get(i) {
        if tk.kind == TokKind::Comment {
            i += 1;
            continue;
        }
        if tk.kind == TokKind::Punct && matches!(tk.text, ">" | "<" | "=" | "!") {
            op.push_str(tk.text);
            i += 1;
            steps += 1;
            if steps > 2 {
                return None;
            }
        } else {
            break;
        }
    }
    if op.is_empty() {
        return None;
    }
    let ni = skip_comments(tokens, i);
    let lit = tokens.get(ni)?;
    if lit.kind != TokKind::Number {
        return None;
    }
    Some((op, lit.text.to_string()))
}

/// For the reverse form `<number> <op> COUNT(*)`: walk back from the COUNT index
/// over the operator run to a leading numeric literal.
fn read_number_then_op_before(tokens: &[Token], count_idx: usize) -> Option<(String, String)> {
    let mut i = count_idx;
    // back up over comments
    let mut op = String::new();
    let mut steps = 0;
    loop {
        if i == 0 {
            return None;
        }
        i -= 1;
        let tk = tokens.get(i)?;
        if tk.kind == TokKind::Comment {
            continue;
        }
        if tk.kind == TokKind::Punct && matches!(tk.text, ">" | "<" | "=" | "!") {
            // operators read right-to-left; prepend
            op.insert_str(0, tk.text);
            steps += 1;
            if steps > 2 {
                return None;
            }
        } else {
            break;
        }
    }
    if op.is_empty() {
        return None;
    }
    let lit = tokens.get(i)?;
    if lit.kind != TokKind::Number {
        return None;
    }
    Some((op, lit.text.to_string()))
}

/// Mirror a comparison operator (left/right operand swap).
fn flip_op(op: &str) -> String {
    match op {
        ">" => "<",
        "<" => ">",
        ">=" => "<=",
        "<=" => ">=",
        other => other, // `=`, `<>`, `!=` are symmetric
    }
    .to_string()
}

/// True only for operator/literal combos that collapse to a yes/no existence
/// boundary. Notably `= 1` is rejected (that's a cardinality assertion).
fn is_existence_boundary(op: &str, lit: &str) -> bool {
    let n: f64 = match lit.parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    match op {
        // "at least one"
        ">" => n == 0.0,
        ">=" => n == 1.0,
        "<>" | "!=" => n == 0.0,
        // "none"
        "<" => n == 1.0,
        "<=" => n == 0.0,
        "=" => n == 0.0, // `= 0` is "none"; `= 1` deliberately excluded
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// SELECT DISTINCT over many columns
// ---------------------------------------------------------------------------

/// A wide `SELECT DISTINCT` (5+ projected columns) is frequently a band-aid
/// masking a JOIN that fans rows out — the author slaps DISTINCT on the result
/// to collapse the duplicates instead of fixing the join cardinality.
///
/// Guard: only fire when the FROM clause is *multi-table* (contains a JOIN /
/// APPLY / comma-separated table list). A wide DISTINCT over a single table is
/// legitimate set-semantics de-duplication (staging / dimension dedup) and must
/// not fire.
pub fn distinct_many_columns(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !kw(t, "SELECT") {
            continue;
        }
        let d = skip_comments(tokens, i + 1);
        if !tokens.get(d).map(|x| kw(x, "DISTINCT")).unwrap_or(false) {
            continue;
        }

        // Count top-level (depth-0) commas in the projection, from after
        // DISTINCT up to the matching FROM of this SELECT.
        let mut j = d + 1;
        let mut depth = 0i32;
        let mut commas = 0usize;
        let mut from_idx: Option<usize> = None;
        while j < tokens.len() {
            let tk = &tokens[j];
            match tk.text {
                "(" => depth += 1,
                ")" => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                ";" if depth == 0 => break,
                "," if depth == 0 => commas += 1,
                _ => {
                    if depth == 0 && kw(tk, "FROM") {
                        from_idx = Some(j);
                        break;
                    }
                }
            }
            j += 1;
        }
        let cols = commas + 1; // N commas → N+1 columns
        if cols < 5 {
            continue;
        }
        let Some(from_idx) = from_idx else { continue };

        // Inspect the FROM clause: is it multi-table?
        if from_clause_is_multi_table(tokens, from_idx) {
            out.push(finding(
                "antipattern.distinct_many_columns",
                Severity::Info,
                format!("SELECT DISTINCT over {cols} columns combined with a multi-table FROM is often a band-aid hiding a join that fans rows out — DISTINCT then re-collapses the duplicates with a sort/hash."),
                Some(make_loc(t)),
                Some("Check whether a join is multiplying rows. Prefer fixing the join (e.g. EXISTS / a properly-keyed join) so the result is naturally unique, instead of masking duplicates with a wide DISTINCT.".into()),
            ));
        }
    }
    out
}

/// Starting at the FROM token index, decide whether the FROM clause lists more
/// than one table source (JOIN / APPLY / a top-level comma between table refs)
/// before the statement terminator.
fn from_clause_is_multi_table(tokens: &[Token], from_idx: usize) -> bool {
    let mut j = from_idx + 1;
    let mut depth = 0i32;
    while j < tokens.len() {
        let tk = &tokens[j];
        match tk.text {
            "(" => depth += 1,
            ")" => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ";" if depth == 0 => break,
            "," if depth == 0 => return true, // old-style comma join
            _ => {
                if depth == 0 {
                    // Stop scanning once we leave the FROM clause.
                    if kw(tk, "WHERE")
                        || kw(tk, "GROUP")
                        || kw(tk, "ORDER")
                        || kw(tk, "HAVING")
                        || kw(tk, "UNION")
                        || kw(tk, "OPTION")
                    {
                        break;
                    }
                    if kw(tk, "JOIN") || kw(tk, "APPLY") {
                        return true;
                    }
                }
            }
        }
        j += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Correlated scalar subquery in the SELECT list
// ---------------------------------------------------------------------------

/// A *correlated* scalar subquery in the projection re-executes once per output
/// row of the outer query (RBAR). The fix is usually `OUTER APPLY` / a join.
///
/// The hard part is distinguishing a *correlated* subquery (references an outer
/// alias → per-row re-execution) from an *uncorrelated* one (filters only on
/// parameters / constants / its own columns → evaluated once and cached). Only
/// the former is a problem. We fire ONLY when the inner WHERE contains a
/// qualified `<alias>.<column>` reference whose leading alias is NOT a table
/// source declared inside the subquery's own FROM — i.e. it resolves to the
/// outer scope.
pub fn correlated_scalar_subquery_in_select(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Walk the projection region of each SELECT (between SELECT and its FROM)
    // looking for `( SELECT … )` scalar subqueries.
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if !kw(t, "SELECT") {
            i += 1;
            continue;
        }
        // A SELECT that is itself the body of a CROSS/OUTER APPLY is already
        // the rewrite this rule recommends.
        if select_is_apply_body(tokens, i) {
            i += 1;
            continue;
        }
        // Find the projection's matching top-level FROM (end of the projection).
        let mut j = i + 1;
        let mut depth = 0i32;
        let mut proj_end = tokens.len();
        while j < tokens.len() {
            let tk = &tokens[j];
            match tk.text {
                "(" => depth += 1,
                ")" => {
                    if depth == 0 {
                        proj_end = j;
                        break;
                    }
                    depth -= 1;
                }
                ";" if depth == 0 => {
                    proj_end = j;
                    break;
                }
                _ => {
                    if depth == 0 && kw(tk, "FROM") {
                        proj_end = j;
                        break;
                    }
                }
            }
            j += 1;
        }

        // Scan the projection [i+1, proj_end) for `( SELECT` scalar subqueries.
        let mut k = i + 1;
        while k < proj_end {
            let tk = &tokens[k];
            if tk.text == "(" {
                let inner = skip_comments(tokens, k + 1);
                if tokens.get(inner).map(|x| kw(x, "SELECT")).unwrap_or(false) {
                    // Find the matching close paren for this subquery.
                    if let Some(close) = matching_paren(tokens, k) {
                        // `CASE WHEN NOT EXISTS (SELECT 1 …)` in the select
                        // list is a semi-join predicate, not a scalar lookup;
                        // and a `FOR XML` subquery (STUFF/CSV idiom, nested
                        // element builders) has no OUTER APPLY equivalent.
                        let skip = paren_is_exists(tokens, k)
                            || subquery_builds_xml(tokens, inner, close);
                        if !skip && subquery_is_correlated(tokens, inner, close) {
                            out.push(finding(
                                "antipattern.correlated_scalar_subquery_in_select",
                                Severity::Warning,
                                "A correlated scalar subquery in the SELECT list re-executes once per output row of the outer query (RBAR) — its filter references an outer-query alias.",
                                Some(make_loc(&tokens[inner])),
                                Some("Rewrite as OUTER APPLY (or a LEFT JOIN to a pre-aggregated derived table) so the engine evaluates the lookup set-based, once, instead of row-by-row.".into()),
                            ));
                        }
                        // Skip past this subquery so we don't double-scan nested ones.
                        k = close + 1;
                        continue;
                    }
                }
            }
            k += 1;
        }

        i = proj_end.max(i + 1);
    }
    out
}

/// Index of the previous non-comment token before `i`.
fn prev_sig(tokens: &[Token], i: usize) -> Option<usize> {
    let mut k = i;
    while k > 0 {
        k -= 1;
        if tokens[k].kind != TokKind::Comment {
            return Some(k);
        }
    }
    None
}

/// Is the `(` at `open` the argument of `EXISTS` / `NOT EXISTS`?
fn paren_is_exists(tokens: &[Token], open: usize) -> bool {
    let mut at = open;
    loop {
        let Some(p) = prev_sig(tokens, at) else { return false };
        if tokens[p].text == "(" {
            at = p;
            continue;
        }
        return kw(&tokens[p], "EXISTS");
    }
}

/// Does the subquery body `[inner, close)` end in a `FOR XML` clause (at any
/// nesting)? Such subqueries build strings or XML nodes, not scalar lookups.
fn subquery_builds_xml(tokens: &[Token], inner: usize, close: usize) -> bool {
    let mut j = inner;
    while j + 1 < close {
        if kw(&tokens[j], "FOR") {
            let n = skip_comments(tokens, j + 1);
            if n < close && kw(&tokens[n], "XML") {
                return true;
            }
        }
        j += 1;
    }
    false
}

/// Is the SELECT at `i` the first token inside a `CROSS APPLY (` /
/// `OUTER APPLY (` derived table?
fn select_is_apply_body(tokens: &[Token], i: usize) -> bool {
    let Some(open) = prev_sig(tokens, i) else { return false };
    if tokens[open].text != "(" {
        return false;
    }
    prev_sig(tokens, open)
        .map(|p| kw(&tokens[p], "APPLY"))
        .unwrap_or(false)
}

/// Index of the `)` matching the `(` at `open`.
fn matching_paren(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = open;
    while j < tokens.len() {
        match tokens[j].text {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// A scalar subquery spanning tokens `(inner_select_idx .. close)` is considered
/// *correlated* iff its (top-level) WHERE contains a qualified `<a>.<b>`
/// reference whose leading identifier `a` is NOT a table source / alias declared
/// inside this subquery's own FROM clause. Such a reference must bind to the
/// outer scope, which is the definition of correlation.
fn subquery_is_correlated(tokens: &[Token], inner_select_idx: usize, close: usize) -> bool {
    // 1. Collect the alias/table names introduced by THIS subquery's FROM/JOINs.
    let mut local: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut where_idx: Option<usize> = None;
    let mut depth = 0i32;
    let mut j = inner_select_idx + 1;
    while j < close {
        let tk = &tokens[j];
        match tk.text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {
                if depth == 0 {
                    if kw(tk, "FROM") || kw(tk, "JOIN") || kw(tk, "APPLY") {
                        // table name immediately follows; possibly schema.table
                        collect_table_and_alias(tokens, j, close, &mut local);
                    } else if kw(tk, "WHERE") {
                        where_idx = Some(j);
                        break;
                    }
                }
            }
        }
        j += 1;
    }

    let Some(where_idx) = where_idx else {
        // No WHERE → cannot be a per-row correlated filter we care about.
        return false;
    };

    // 2. Scan the WHERE region [where_idx+1, close) for a qualified reference
    //    `<ident> . <ident>` whose leading ident is not local.
    let mut depth = 0i32;
    let mut k = where_idx + 1;
    while k < close {
        let tk = &tokens[k];
        match tk.text {
            "(" => depth += 1,
            ")" => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
        // qualified ref: Word '.' Word, where the leading Word is an alias
        if tk.kind == TokKind::Word
            && !tk.text.starts_with('@')
            && !tk.text.starts_with('#')
        {
            let dot = tokens.get(k + 1);
            let col = tokens.get(k + 2);
            if dot.map(|d| d.text == ".").unwrap_or(false)
                && col.map(|c| c.kind == TokKind::Word).unwrap_or(false)
            {
                let alias = ident_text(tk).to_ascii_lowercase();
                if !local.contains(&alias) {
                    return true;
                }
            }
        }
        k += 1;
    }
    false
}

/// After a FROM/JOIN/APPLY keyword at `kw_idx`, record the table name and its
/// alias (the local correlation names) into `local`.
fn collect_table_and_alias(
    tokens: &[Token],
    kw_idx: usize,
    close: usize,
    local: &mut std::collections::HashSet<String>,
) {
    let mut p = skip_comments(tokens, kw_idx + 1);
    if p >= close {
        return;
    }
    // table reference: [schema .] name  (possibly delimited)
    if tokens[p].kind != TokKind::Word {
        return;
    }
    // consume schema.name chain
    let mut last_name_idx = p;
    loop {
        let dot = p + 1;
        let nxt = p + 2;
        if tokens.get(dot).map(|d| d.text == ".").unwrap_or(false)
            && tokens.get(nxt).map(|n| n.kind == TokKind::Word).unwrap_or(false)
        {
            p = nxt;
            last_name_idx = p;
        } else {
            break;
        }
    }
    // base table name is also a usable correlation name
    local.insert(ident_text(&tokens[last_name_idx]).to_ascii_lowercase());

    // optional alias: [AS] <ident>
    let mut a = skip_comments(tokens, last_name_idx + 1);
    if a < close && tokens[a].kind == TokKind::Word && kw(&tokens[a], "AS") {
        a = skip_comments(tokens, a + 1);
    }
    if a < close
        && tokens[a].kind == TokKind::Word
        && !is_clause_kw(&tokens[a])
        && !tokens[a].text.starts_with('@')
    {
        local.insert(ident_text(&tokens[a]).to_ascii_lowercase());
    }
}

fn is_clause_kw(t: &Token) -> bool {
    kw(t, "WHERE")
        || kw(t, "GROUP")
        || kw(t, "ORDER")
        || kw(t, "HAVING")
        || kw(t, "JOIN")
        || kw(t, "INNER")
        || kw(t, "LEFT")
        || kw(t, "RIGHT")
        || kw(t, "FULL")
        || kw(t, "CROSS")
        || kw(t, "OUTER")
        || kw(t, "ON")
        || kw(t, "APPLY")
        || kw(t, "UNION")
        || kw(t, "OPTION")
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
mod tests {
    use crate::{analyze, AnalyzeInput};
    use std::collections::HashSet;

    fn fired(sql: &str) -> HashSet<String> {
        let input = AnalyzeInput {
            sql: Some(sql.to_string()),
            server_version: Some(2025),
            ..Default::default()
        };
        analyze(&input)
            .findings
            .into_iter()
            .map(|f| f.rule.0)
            .collect()
    }

    // -- union_should_be_union_all --------------------------------------

    /// FP: a delimited identifier literally named `[UNION]` must NOT be treated
    /// as the UNION keyword.
    #[test]
    fn fp_quoted_union_identifier_does_not_fire() {
        let f = fired("SELECT [UNION] FROM dbo.Settings;");
        assert!(
            !f.contains("antipattern.union_should_be_union_all"),
            "[UNION] is an identifier, not the keyword: {f:?}"
        );
    }

    /// Positive: a real UNION (no ALL) between two SELECTs still fires.
    #[test]
    fn pos_plain_union_fires() {
        let f = fired("SELECT a FROM t1 UNION SELECT a FROM t2;");
        assert!(
            f.contains("antipattern.union_should_be_union_all"),
            "plain UNION should fire: {f:?}"
        );
    }

    /// UNION ALL is already correct and must not fire.
    #[test]
    fn neg_union_all_does_not_fire() {
        let f = fired("SELECT a FROM t1 UNION ALL SELECT a FROM t2;");
        assert!(
            !f.contains("antipattern.union_should_be_union_all"),
            "UNION ALL must not fire: {f:?}"
        );
    }

    // -- count_for_existence --------------------------------------------

    /// FP: `COUNT(*) = 1` is a cardinality check ("exactly one"), NOT an
    /// existence test — EXISTS cannot replace it, so the rule must stay silent.
    #[test]
    fn fp_count_exactly_one_does_not_fire() {
        let f = fired("IF (SELECT COUNT(*) FROM Users WHERE Email = @e) = 1 SET @ok = 1;");
        assert!(
            !f.contains("antipattern.count_for_existence"),
            "COUNT(*) = 1 is a cardinality check, must not fire: {f:?}"
        );
    }

    /// Positive: `COUNT(*) > 0` is a genuine existence test.
    #[test]
    fn pos_count_greater_than_zero_fires() {
        let f = fired("IF (SELECT COUNT(*) FROM Users WHERE Email = @e) > 0 SET @ok = 1;");
        assert!(
            f.contains("antipattern.count_for_existence"),
            "COUNT(*) > 0 should fire: {f:?}"
        );
    }

    /// Positive: `COUNT(*) = 0` ("none") is an existence test.
    #[test]
    fn pos_count_equals_zero_fires() {
        let f = fired("IF (SELECT COUNT(*) FROM Users WHERE Active = 1) = 0 RETURN;");
        assert!(
            f.contains("antipattern.count_for_existence"),
            "COUNT(*) = 0 should fire: {f:?}"
        );
    }

    /// Positive: `COUNT(*) >= 1` ("at least one") is an existence test.
    #[test]
    fn pos_count_ge_one_fires() {
        let f = fired("IF (SELECT COUNT(*) FROM Users WHERE Active = 1) >= 1 RETURN;");
        assert!(
            f.contains("antipattern.count_for_existence"),
            "COUNT(*) >= 1 should fire: {f:?}"
        );
    }

    // -- distinct_many_columns ------------------------------------------

    /// FP: a wide SELECT DISTINCT over a SINGLE table is legitimate dedup.
    #[test]
    fn fp_distinct_single_table_dedup_does_not_fire() {
        let f = fired("SELECT DISTINCT Year, Quarter, Month, Week, Day FROM CalendarStaging;");
        assert!(
            !f.contains("antipattern.distinct_many_columns"),
            "single-table wide DISTINCT is legit dedup, must not fire: {f:?}"
        );
    }

    /// Positive: a wide DISTINCT over a multi-table JOIN fires.
    #[test]
    fn pos_distinct_many_columns_with_join_fires() {
        let f = fired(
            "SELECT DISTINCT a.c1, a.c2, b.c3, b.c4, b.c5 FROM A a JOIN B b ON b.aid = a.id;",
        );
        assert!(
            f.contains("antipattern.distinct_many_columns"),
            "wide DISTINCT over a JOIN should fire: {f:?}"
        );
    }

    /// Negative: wide DISTINCT over a comma-join (old style) also fires (multi-table).
    #[test]
    fn pos_distinct_many_columns_comma_join_fires() {
        let f = fired("SELECT DISTINCT a.c1, a.c2, b.c3, b.c4, b.c5 FROM A a, B b WHERE b.aid = a.id;");
        assert!(
            f.contains("antipattern.distinct_many_columns"),
            "wide DISTINCT over comma-join should fire: {f:?}"
        );
    }

    // -- correlated_scalar_subquery_in_select ---------------------------

    /// FP: uncorrelated scalar subquery whose WHERE filters only on a parameter.
    #[test]
    fn fp_uncorrelated_param_where_does_not_fire() {
        let f = fired(
            "SELECT u.Id, (SELECT MAX(Price) FROM Products WHERE CategoryId = @cat) AS MaxInCat FROM Users u;",
        );
        assert!(
            !f.contains("antipattern.correlated_scalar_subquery_in_select"),
            "param-filtered (uncorrelated) subquery must not fire: {f:?}"
        );
    }

    /// FP: uncorrelated, filtered on a constant literal.
    #[test]
    fn fp_uncorrelated_const_where_does_not_fire() {
        let f = fired(
            "SELECT l.Id, (SELECT SUM(Amount) FROM Ledger WHERE PostedYear = 2025) AS Tot FROM Lines l;",
        );
        assert!(
            !f.contains("antipattern.correlated_scalar_subquery_in_select"),
            "constant-filtered (uncorrelated) subquery must not fire: {f:?}"
        );
    }

    /// FP: uncorrelated subquery whose only qualified ref is its OWN alias.
    #[test]
    fn fp_uncorrelated_own_alias_does_not_fire() {
        let f = fired(
            "SELECT u.Id, (SELECT MAX(p.Price) FROM Products p WHERE p.CategoryId = 7) AS M FROM Users u;",
        );
        assert!(
            !f.contains("antipattern.correlated_scalar_subquery_in_select"),
            "subquery referencing only its own alias is uncorrelated: {f:?}"
        );
    }

    /// Positive: a genuinely correlated scalar subquery (references outer alias `u`).
    #[test]
    fn pos_correlated_subquery_fires() {
        let f = fired(
            "SELECT u.Id, (SELECT MAX(o.Total) FROM Orders o WHERE o.UserId = u.Id) AS MaxOrder FROM Users u;",
        );
        assert!(
            f.contains("antipattern.correlated_scalar_subquery_in_select"),
            "correlated subquery (refs outer u.Id) should fire: {f:?}"
        );
    }
}
