// Index-design rules: clustered key shape, columnstore opportunity, filtered index hints.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Strip surrounding [] brackets from a Word token's text for name comparisons.
fn bare_name<'a>(t: &'a Token<'a>) -> &'a str {
    t.text.trim_matches(|c| c == '[' || c == ']')
}

fn name_eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

fn name_contains_ci(name: &str, needle: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains(&needle.to_ascii_lowercase())
}

/// Find next non-comment token index >= from.
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment { k += 1; }
    k
}

/// Locate `CREATE TABLE` statements and return (open_paren_idx, close_paren_idx)
/// of the column-list. Skips comments. Returns empty list if none found.
fn create_table_bodies(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "CREATE") { i += 1; continue; }
        let j = skip_comments(tokens, i + 1);
        if j >= tokens.len() { break; }
        if !is_word(&tokens[j], "TABLE") { i += 1; continue; }
        // Find the first '(' after this.
        let mut k = j + 1;
        while k < tokens.len() && tokens[k].text != "(" { k += 1; }
        if k >= tokens.len() { break; }
        // Find matching ')'.
        let mut depth = 0i32;
        let mut m = k;
        while m < tokens.len() {
            if tokens[m].text == "(" { depth += 1; }
            else if tokens[m].text == ")" {
                depth -= 1;
                if depth == 0 { break; }
            }
            m += 1;
        }
        if m < tokens.len() { out.push((k, m)); i = m + 1; } else { break; }
    }
    out
}

/// Within a column-list body (open, close), split into "items" separated by top-level commas.
/// Returns Vec of (start_idx, end_idx_exclusive) ranges inside `tokens`, NOT including the comma.
fn split_column_list(tokens: &[Token<'_>], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let mut i = open + 1;
    while i < close {
        let t = &tokens[i];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.text == "," {
            out.push((start, i));
            start = i + 1;
        }
        i += 1;
    }
    if start < close { out.push((start, close)); }
    out
}

/// True if this column-list item is a constraint (begins with CONSTRAINT or PRIMARY/UNIQUE/FOREIGN/CHECK).
fn item_is_constraint(tokens: &[Token<'_>], start: usize, end: usize) -> bool {
    let mut i = skip_comments(tokens, start);
    if i >= end { return false; }
    if is_word(&tokens[i], "CONSTRAINT") {
        // skip CONSTRAINT <name>
        i = skip_comments(tokens, i + 1);
        if i < end && tokens[i].kind == TokKind::Word { i = skip_comments(tokens, i + 1); }
    }
    if i >= end { return false; }
    let t = &tokens[i];
    is_word(t, "PRIMARY")
        || is_word(t, "UNIQUE")
        || is_word(t, "FOREIGN")
        || is_word(t, "CHECK")
        || is_word(t, "INDEX")
}

/// Returns true if the range contains the sequence `WORD ... WORD` for both kws in order.
fn range_has_seq(tokens: &[Token<'_>], start: usize, end: usize, a: &str, b: &str) -> bool {
    let mut i = start;
    while i < end {
        if is_word(&tokens[i], a) {
            let mut j = skip_comments(tokens, i + 1);
            // tolerate one extra word in between? spec wants adjacency; we'll be strict-adjacent.
            if j < end && is_word(&tokens[j], b) { return true; }
        }
        i += 1;
    }
    false
}

/// Returns true if range contains the given keyword (case-insensitive Word match).
fn range_has_kw(tokens: &[Token<'_>], start: usize, end: usize, kw: &str) -> bool {
    (start..end).any(|i| is_word(&tokens[i], kw))
}

pub fn guid_clustered_key(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Part A: CREATE TABLE column definitions.
    for (open, close) in create_table_bodies(tokens) {
        // Collect column-name -> is_uniqueidentifier
        let items = split_column_list(tokens, open, close);
        let mut col_types: Vec<(String, bool)> = Vec::new();
        for &(s, e) in &items {
            if item_is_constraint(tokens, s, e) { continue; }
            let i = skip_comments(tokens, s);
            if i >= e || tokens[i].kind != TokKind::Word { continue; }
            let col_name = bare_name(&tokens[i]).to_string();
            // Find type word — next word.
            let mut j = skip_comments(tokens, i + 1);
            if j >= e { continue; }
            let is_guid = is_word(&tokens[j], "uniqueidentifier")
                || (tokens[j].kind == TokKind::Word
                    && name_eq_ci(bare_name(&tokens[j]), "uniqueidentifier"));
            col_types.push((col_name.clone(), is_guid));

            // Inline `PRIMARY KEY CLUSTERED` on a uniqueidentifier column.
            if is_guid && range_has_seq(tokens, j + 1, e, "PRIMARY", "KEY")
                && range_has_kw(tokens, j + 1, e, "CLUSTERED")
            {
                out.push(finding(
                    "index.guid_clustered_key",
                    Severity::Warning,
                    format!("Column `{}` is uniqueidentifier and is the clustered primary key — random GUID inserts cause page splits.", col_name),
                    Some(make_loc(&tokens[i])),
                    Some("Random GUID inserts cause page splits and high fragmentation. Cluster on an `IDENTITY` BIGINT instead and keep the GUID as a nonclustered unique key, or use `NEWSEQUENTIALID()` for the default.".into()),
                ));
            }
        }

        // Part A2: table-level `PRIMARY KEY CLUSTERED (<col>)` referring to a uniqueidentifier column.
        for &(s, e) in &items {
            if !item_is_constraint(tokens, s, e) { continue; }
            // Look for PRIMARY KEY CLUSTERED ( <ident> )
            let mut k = s;
            let mut found_seq = false;
            while k + 2 < e {
                if is_word(&tokens[k], "PRIMARY")
                    && is_word(&tokens[skip_comments(tokens, k + 1)], "KEY")
                {
                    let kk = skip_comments(tokens, skip_comments(tokens, k + 1) + 1);
                    if kk < e && is_word(&tokens[kk], "CLUSTERED") {
                        found_seq = true;
                        // find '('
                        let mut p = kk + 1;
                        while p < e && tokens[p].text != "(" { p += 1; }
                        if p < e {
                            let q = skip_comments(tokens, p + 1);
                            if q < e && tokens[q].kind == TokKind::Word {
                                let col_name = bare_name(&tokens[q]);
                                let is_guid_col = col_types.iter().any(|(n, g)| *g && name_eq_ci(n, col_name));
                                if is_guid_col {
                                    out.push(finding(
                                        "index.guid_clustered_key",
                                        Severity::Warning,
                                        format!("Clustered primary key references uniqueidentifier column `{}` — random GUID inserts cause page splits.", col_name),
                                        Some(make_loc(&tokens[q])),
                                        Some("Random GUID inserts cause page splits and high fragmentation. Cluster on an `IDENTITY` BIGINT instead and keep the GUID as a nonclustered unique key, or use `NEWSEQUENTIALID()` for the default.".into()),
                                    ));
                                }
                            }
                        }
                        break;
                    }
                }
                k += 1;
            }
            let _ = found_seq;
        }
    }

    // Part B: CREATE CLUSTERED INDEX ... ON <table> (<col>) where col name hints GUID.
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "CREATE") { i += 1; continue; }
        let j = skip_comments(tokens, i + 1);
        // optional UNIQUE
        let mut k = j;
        if k < tokens.len() && is_word(&tokens[k], "UNIQUE") { k = skip_comments(tokens, k + 1); }
        if k >= tokens.len() || !is_word(&tokens[k], "CLUSTERED") { i += 1; continue; }
        let m = skip_comments(tokens, k + 1);
        if m >= tokens.len() || !is_word(&tokens[m], "INDEX") { i += 1; continue; }
        // Find first '(' — column list.
        let mut p = m + 1;
        while p < tokens.len() && tokens[p].text != "(" && tokens[p].text != ";" { p += 1; }
        if p >= tokens.len() || tokens[p].text == ";" { i = m + 1; continue; }
        let q = skip_comments(tokens, p + 1);
        if q < tokens.len() && tokens[q].kind == TokKind::Word {
            let col_name = bare_name(&tokens[q]);
            let hint = name_contains_ci(col_name, "guid")
                || name_contains_ci(col_name, "uniqueidentifier")
                || (name_contains_ci(col_name, "uid") && !name_contains_ci(col_name, "build"));
            if hint {
                out.push(finding(
                    "index.guid_clustered_key",
                    Severity::Warning,
                    format!("CLUSTERED INDEX on column `{}` whose name suggests a GUID — random inserts will fragment the table.", col_name),
                    Some(make_loc(&tokens[q])),
                    Some("Random GUID inserts cause page splits and high fragmentation. Cluster on an `IDENTITY` BIGINT instead and keep the GUID as a nonclustered unique key, or use `NEWSEQUENTIALID()` for the default.".into()),
                ));
            }
        }
        i = p + 1;
    }

    out
}

pub fn wide_clustered_key(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    for (open, close) in create_table_bodies(tokens) {
        let items = split_column_list(tokens, open, close);

        // Build column -> type info for single-column key checks.
        let mut col_info: Vec<(String, String, Option<u32>, usize)> = Vec::new();
        // (col_name, type_name_lower, optional_n, name_token_idx)
        for &(s, e) in &items {
            if item_is_constraint(tokens, s, e) { continue; }
            let i = skip_comments(tokens, s);
            if i >= e || tokens[i].kind != TokKind::Word { continue; }
            let col_name = bare_name(&tokens[i]).to_string();
            let j = skip_comments(tokens, i + 1);
            if j >= e || tokens[j].kind != TokKind::Word { continue; }
            let type_name = bare_name(&tokens[j]).to_ascii_lowercase();
            // optional (N)
            let mut n_val: Option<u32> = None;
            let k = skip_comments(tokens, j + 1);
            if k < e && tokens[k].text == "(" {
                let nn = skip_comments(tokens, k + 1);
                if nn < e && tokens[nn].kind == TokKind::Number {
                    n_val = tokens[nn].text.parse::<u32>().ok();
                }
            }
            col_info.push((col_name, type_name, n_val, i));
        }

        // Process constraints — wide composite PK CLUSTERED or wide-type single-column PK CLUSTERED.
        for &(s, e) in &items {
            // Inline column-level `PRIMARY KEY CLUSTERED` on a single wide string column.
            if !item_is_constraint(tokens, s, e) {
                // Check if this column line has PRIMARY KEY CLUSTERED.
                let has_pk = range_has_seq(tokens, s, e, "PRIMARY", "KEY");
                let has_clu = range_has_kw(tokens, s, e, "CLUSTERED");
                if has_pk && has_clu {
                    let i = skip_comments(tokens, s);
                    if i < e && tokens[i].kind == TokKind::Word {
                        let col_name = bare_name(&tokens[i]).to_string();
                        let j = skip_comments(tokens, i + 1);
                        if j < e && tokens[j].kind == TokKind::Word {
                            let type_name = bare_name(&tokens[j]).to_ascii_lowercase();
                            let is_string_type = matches!(type_name.as_str(),
                                "nvarchar" | "varchar" | "char" | "nchar");
                            let mut n_val: Option<u32> = None;
                            let k = skip_comments(tokens, j + 1);
                            if k < e && tokens[k].text == "(" {
                                let nn = skip_comments(tokens, k + 1);
                                if nn < e && tokens[nn].kind == TokKind::Number {
                                    n_val = tokens[nn].text.parse::<u32>().ok();
                                }
                            }
                            if is_string_type && n_val.map(|n| n > 32).unwrap_or(false) {
                                out.push(finding(
                                    "index.wide_clustered_key",
                                    Severity::Warning,
                                    format!("Clustered key on wide {}({}) column `{}` — every nonclustered index will inherit this width.", type_name, n_val.unwrap(), col_name),
                                    Some(make_loc(&tokens[i])),
                                    Some("Wide clustered keys auto-append to every nonclustered index, inflating storage. Use a narrow surrogate `INT`/`BIGINT IDENTITY` clustered key; enforce business uniqueness with separate UNIQUE constraints.".into()),
                                ));
                            }
                        }
                    }
                }
                continue;
            }

            // Table-level constraint: PRIMARY KEY CLUSTERED ( col1, col2, ... )
            let mut k = s;
            let mut found = false;
            while k < e {
                if is_word(&tokens[k], "PRIMARY") {
                    let kn = skip_comments(tokens, k + 1);
                    if kn < e && is_word(&tokens[kn], "KEY") {
                        let kc = skip_comments(tokens, kn + 1);
                        if kc < e && is_word(&tokens[kc], "CLUSTERED") {
                            found = true;
                            // Find '(' then count idents.
                            let mut p = kc + 1;
                            while p < e && tokens[p].text != "(" { p += 1; }
                            if p < e {
                                // Find matching ')'.
                                let mut depth = 0i32;
                                let mut q = p;
                                while q < e {
                                    if tokens[q].text == "(" { depth += 1; }
                                    else if tokens[q].text == ")" {
                                        depth -= 1;
                                        if depth == 0 { break; }
                                    }
                                    q += 1;
                                }
                                // Count comma-separated entries.
                                let mut cols: Vec<String> = Vec::new();
                                let mut depth2 = 0i32;
                                let mut item_start = p + 1;
                                let mut idx = p + 1;
                                while idx < q {
                                    let tt = &tokens[idx];
                                    if tt.text == "(" { depth2 += 1; }
                                    else if tt.text == ")" { depth2 -= 1; }
                                    else if depth2 == 0 && tt.text == "," {
                                        // first word in [item_start, idx)
                                        let mut cc = item_start;
                                        while cc < idx && tokens[cc].kind != TokKind::Word { cc += 1; }
                                        if cc < idx { cols.push(bare_name(&tokens[cc]).to_string()); }
                                        item_start = idx + 1;
                                    }
                                    idx += 1;
                                }
                                if item_start < q {
                                    let mut cc = item_start;
                                    while cc < q && tokens[cc].kind != TokKind::Word { cc += 1; }
                                    if cc < q { cols.push(bare_name(&tokens[cc]).to_string()); }
                                }
                                if cols.len() >= 3 {
                                    out.push(finding(
                                        "index.wide_clustered_key",
                                        Severity::Warning,
                                        format!("Composite clustered key has {} columns — every nonclustered index will append all of them.", cols.len()),
                                        Some(make_loc(&tokens[kc])),
                                        Some("Wide clustered keys auto-append to every nonclustered index, inflating storage. Use a narrow surrogate `INT`/`BIGINT IDENTITY` clustered key; enforce business uniqueness with separate UNIQUE constraints.".into()),
                                    ));
                                } else if cols.len() == 1 {
                                    // Single-column wide-string check via col_info.
                                    if let Some((_, type_name, n_val, name_idx)) = col_info
                                        .iter()
                                        .find(|(n, _, _, _)| name_eq_ci(n, &cols[0]))
                                    {
                                        let is_string_type = matches!(type_name.as_str(),
                                            "nvarchar" | "varchar" | "char" | "nchar");
                                        if is_string_type && n_val.map(|n| n > 32).unwrap_or(false) {
                                            out.push(finding(
                                                "index.wide_clustered_key",
                                                Severity::Warning,
                                                format!("Clustered key on wide {}({}) column `{}` — every nonclustered index will inherit this width.", type_name, n_val.unwrap(), cols[0]),
                                                Some(make_loc(&tokens[*name_idx])),
                                                Some("Wide clustered keys auto-append to every nonclustered index, inflating storage. Use a narrow surrogate `INT`/`BIGINT IDENTITY` clustered key; enforce business uniqueness with separate UNIQUE constraints.".into()),
                                            ));
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                k += 1;
            }
            let _ = found;
        }
    }

    out
}

pub fn columnstore_candidate_aggregating_scan(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "SELECT") { i += 1; continue; }
        // Find statement end.
        let stmt_start = i;
        let mut depth = 0i32;
        let mut j = i + 1;
        let mut stmt_end = tokens.len();
        while j < tokens.len() {
            let t = &tokens[j];
            if t.text == "(" { depth += 1; }
            else if t.text == ")" { depth -= 1; if depth < 0 { stmt_end = j; break; } }
            else if depth == 0 && t.text == ";" { stmt_end = j; break; }
            j += 1;
        }

        // Skip TOP (...) statements.
        let mut has_top = false;
        let mut p = stmt_start + 1;
        while p < stmt_end && p < stmt_start + 4 {
            if is_word(&tokens[p], "TOP") {
                let pn = skip_comments(tokens, p + 1);
                if pn < stmt_end && tokens[pn].text == "(" { has_top = true; break; }
            }
            p += 1;
        }
        if has_top { i = stmt_end + 1; continue; }

        // Find aggregate call: WORD followed by '(' where WORD is SUM/COUNT/AVG/MIN/MAX.
        let mut has_agg = false;
        let mut agg_tok_idx = 0usize;
        let mut k = stmt_start;
        while k < stmt_end {
            let t = &tokens[k];
            if t.kind == TokKind::Word {
                let u = bare_name(t).to_ascii_uppercase();
                if matches!(u.as_str(), "SUM" | "COUNT" | "AVG" | "MIN" | "MAX") {
                    let kn = skip_comments(tokens, k + 1);
                    if kn < stmt_end && tokens[kn].text == "(" {
                        has_agg = true;
                        agg_tok_idx = k;
                        break;
                    }
                }
            }
            k += 1;
        }
        if !has_agg { i = stmt_end + 1; continue; }

        // Find GROUP BY in same statement (top-level).
        let mut has_group_by = false;
        let mut d2 = 0i32;
        let mut kk = stmt_start;
        while kk < stmt_end {
            let t = &tokens[kk];
            if t.text == "(" { d2 += 1; }
            else if t.text == ")" { d2 -= 1; }
            else if d2 == 0 && is_word(t, "GROUP") {
                let nn = skip_comments(tokens, kk + 1);
                if nn < stmt_end && is_word(&tokens[nn], "BY") { has_group_by = true; break; }
            }
            kk += 1;
        }
        if !has_group_by { i = stmt_end + 1; continue; }

        // Approximate "single fact table" — find top-level FROM, scan until JOIN/WHERE/GROUP at depth 0.
        let mut from_idx: Option<usize> = None;
        let mut d3 = 0i32;
        let mut a = stmt_start;
        while a < stmt_end {
            let t = &tokens[a];
            if t.text == "(" { d3 += 1; }
            else if t.text == ")" { d3 -= 1; }
            else if d3 == 0 && is_word(t, "FROM") { from_idx = Some(a); break; }
            a += 1;
        }
        let from_i = match from_idx { Some(x) => x, None => { i = stmt_end + 1; continue; } };

        let mut d4 = 0i32;
        let mut word_count = 0u32;
        let mut sees_join = false;
        let mut b = from_i + 1;
        while b < stmt_end {
            let t = &tokens[b];
            if t.text == "(" { d4 += 1; }
            else if t.text == ")" { d4 -= 1; }
            else if d4 == 0 {
                if is_word(t, "JOIN") || is_word(t, "INNER") || is_word(t, "LEFT")
                    || is_word(t, "RIGHT") || is_word(t, "FULL") || is_word(t, "CROSS")
                {
                    sees_join = true;
                    break;
                }
                if is_word(t, "WHERE") || is_word(t, "GROUP") || is_word(t, "ORDER")
                    || is_word(t, "HAVING")
                {
                    break;
                }
                if t.kind == TokKind::Word {
                    // Skip aliases prefixed AS.
                    if !is_word(t, "AS") && !is_word(t, "WITH") { word_count += 1; }
                }
            }
            b += 1;
        }
        // A single schema-qualified ident produces Word(schema) Punct(.) Word(name) = 2 words.
        // A bare ident is 1 word. An alias (after AS) ignored.
        // Accept 1, 2 (schema.name), or 3 (db.schema.name) as "single table".
        let single_table = !sees_join && word_count >= 1 && word_count <= 3;

        if single_table {
            out.push(finding(
                "index.missing_columnstore_opportunity",
                Severity::Info,
                "Aggregating scan with GROUP BY on what appears to be a single fact table — classic columnstore use case.",
                Some(make_loc(&tokens[agg_tok_idx])),
                Some("Wide analytical scans over a rowstore table are a classic columnstore use case. Clustered columnstore (`CREATE CLUSTERED COLUMNSTORE INDEX ...`) gives ~10x compression and ~10x scan speed for DW. NCCI (2016+) for HTAP overlay.".into()),
            ));
        }

        i = stmt_end + 1;
    }
    out
}

pub fn filtered_index_opportunity(ctx: &RuleCtx) -> Vec<Finding> {
    let tokens = ctx.tokens;
    // Fire only once per file.
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "WHERE") { i += 1; continue; }
        // Find statement end.
        let mut depth = 0i32;
        let mut j = i + 1;
        let mut where_end = tokens.len();
        while j < tokens.len() {
            let t = &tokens[j];
            if t.text == "(" { depth += 1; }
            else if t.text == ")" { depth -= 1; if depth < 0 { where_end = j; break; } }
            else if depth == 0 && t.text == ";" { where_end = j; break; }
            else if depth == 0 && (is_word(t, "GROUP") || is_word(t, "ORDER") || is_word(t, "HAVING") || is_word(t, "OPTION")) {
                where_end = j; break;
            }
            j += 1;
        }

        // Check for AND/OR at depth 0 in the WHERE — if present, skip (we want a single predicate).
        let mut has_chain = false;
        let mut d2 = 0i32;
        let mut k = i + 1;
        while k < where_end {
            let t = &tokens[k];
            if t.text == "(" { d2 += 1; }
            else if t.text == ")" { d2 -= 1; }
            else if d2 == 0 && (is_word(t, "AND") || is_word(t, "OR")) { has_chain = true; break; }
            k += 1;
        }
        if has_chain { i = where_end + 1; continue; }

        // Scan the predicate: skip leading parens/comments, expect <ident> [= 0 | IS NULL | = 'Y'/'N'].
        let mut p = i + 1;
        while p < where_end && (tokens[p].kind == TokKind::Comment || tokens[p].text == "(") { p += 1; }
        if p >= where_end || tokens[p].kind != TokKind::Word {
            i = where_end + 1;
            continue;
        }
        let ident_idx = p;
        // Optionally consume `<ident>.<ident>` qualifications.
        let mut q = p + 1;
        while q + 1 < where_end && tokens[q].text == "." && tokens[q + 1].kind == TokKind::Word {
            q += 2;
        }
        // skip comments
        while q < where_end && tokens[q].kind == TokKind::Comment { q += 1; }
        if q >= where_end { i = where_end + 1; continue; }

        let matched = if tokens[q].text == "=" {
            let r = skip_comments(tokens, q + 1);
            if r < where_end {
                let rt = &tokens[r];
                let is_zero = rt.kind == TokKind::Number && rt.text == "0";
                let is_yn = rt.kind == TokKind::String && {
                    let s = rt.text.trim_matches('\'');
                    s.eq_ignore_ascii_case("Y") || s.eq_ignore_ascii_case("N")
                };
                is_zero || is_yn
            } else { false }
        } else if is_word(&tokens[q], "IS") {
            let r = skip_comments(tokens, q + 1);
            r < where_end && is_word(&tokens[r], "NULL")
        } else {
            false
        };

        if matched {
            return vec![finding(
                "index.filtered_index_opportunity_soft_delete",
                Severity::Info,
                "Single-column equality/null predicate on a WHERE — candidate for a filtered nonclustered index.",
                Some(make_loc(&tokens[ident_idx])),
                Some("Filtered indexes are smaller, have accurate filtered statistics, and reduce maintenance for soft-delete / open-row patterns. `CREATE NONCLUSTERED INDEX ... WHERE <col> = 0;` - note filtered indexes only get used for parameterized queries when the literal matches.".into()),
            )];
        }

        i = where_end + 1;
    }
    Vec::new()
}

pub fn clustered_index_guid_no_fillfactor(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "CREATE") { i += 1; continue; }
        let mut k = skip_comments(tokens, i + 1);
        if k < tokens.len() && is_word(&tokens[k], "UNIQUE") { k = skip_comments(tokens, k + 1); }
        if k >= tokens.len() || !is_word(&tokens[k], "CLUSTERED") { i += 1; continue; }
        let m = skip_comments(tokens, k + 1);
        if m >= tokens.len() || !is_word(&tokens[m], "INDEX") { i += 1; continue; }

        // Find '(' (column list) and end-of-statement ';'.
        let mut p = m + 1;
        let mut stmt_end = tokens.len();
        while p < tokens.len() && tokens[p].text != "(" {
            if tokens[p].text == ";" { stmt_end = p; break; }
            p += 1;
        }
        if p >= tokens.len() || tokens[p].text == ";" { i = m + 1; continue; }

        // First column name.
        let q = skip_comments(tokens, p + 1);
        if q >= tokens.len() || tokens[q].kind != TokKind::Word {
            i = p + 1; continue;
        }
        let col_name = bare_name(&tokens[q]).to_string();
        let lower = col_name.to_ascii_lowercase();
        let guid_hint = lower.contains("guid")
            || lower.contains("uniqueidentifier")
            || lower.ends_with("_id")
            || lower.ends_with("uid");
        if !guid_hint { i = p + 1; continue; }

        // Find end of statement.
        let mut depth = 0i32;
        let mut e = p;
        while e < tokens.len() {
            let t = &tokens[e];
            if t.text == "(" { depth += 1; }
            else if t.text == ")" { depth -= 1; }
            else if depth == 0 && t.text == ";" { stmt_end = e; break; }
            e += 1;
        }
        if stmt_end == tokens.len() { stmt_end = tokens.len(); }

        // Check for FILLFACTOR in stmt range.
        let mut has_ff = false;
        for n in m..stmt_end {
            if is_word(&tokens[n], "FILLFACTOR") { has_ff = true; break; }
        }
        if !has_ff {
            out.push(finding(
                "ddl.fillfactor_default_zero_on_random_inserts",
                Severity::Info,
                format!("CLUSTERED INDEX on `{}` looks GUID-like and has no FILLFACTOR — random inserts will cause page splits.", col_name),
                Some(make_loc(&tokens[q])),
                Some("GUID clustered keys need `WITH (FILLFACTOR = 80)` (or similar) to leave page slack and avoid page splits on random inserts.".into()),
            ));
        }

        i = if stmt_end > i { stmt_end + 1 } else { i + 1 };
    }
    out
}

pub fn nullable_columns_should_be_explicit(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Common scalar types we'll recognize.
    let known_types: &[&str] = &[
        "int", "bigint", "smallint", "tinyint",
        "bit", "decimal", "numeric", "money", "smallmoney",
        "float", "real",
        "date", "time", "datetime", "datetime2", "smalldatetime", "datetimeoffset",
        "char", "varchar", "text",
        "nchar", "nvarchar", "ntext",
        "binary", "varbinary", "image",
        "uniqueidentifier", "xml", "sql_variant", "rowversion", "timestamp", "hierarchyid",
    ];

    for (open, close) in create_table_bodies(tokens) {
        let items = split_column_list(tokens, open, close);
        for &(s, e) in &items {
            if out.len() >= 3 { return out; }
            if item_is_constraint(tokens, s, e) { continue; }
            let i = skip_comments(tokens, s);
            if i >= e || tokens[i].kind != TokKind::Word { continue; }
            let j = skip_comments(tokens, i + 1);
            if j >= e || tokens[j].kind != TokKind::Word { continue; }
            let type_name = bare_name(&tokens[j]).to_ascii_lowercase();
            if !known_types.iter().any(|k| k.eq_ignore_ascii_case(&type_name)) { continue; }

            // Scan rest of column item for NULL or NOT NULL at top level.
            let mut k = j + 1;
            // skip optional (N) or (N, M).
            if k < e && tokens[k].text == "(" {
                let mut depth = 0i32;
                while k < e {
                    if tokens[k].text == "(" { depth += 1; }
                    else if tokens[k].text == ")" { depth -= 1; if depth == 0 { k += 1; break; } }
                    k += 1;
                }
            }

            let mut depth = 0i32;
            let mut saw_null = false;
            let mut p = k;
            while p < e {
                let t = &tokens[p];
                if t.text == "(" { depth += 1; }
                else if t.text == ")" { depth -= 1; }
                else if depth == 0 {
                    if is_word(t, "NULL") { saw_null = true; break; }
                    if is_word(t, "NOT") {
                        let n = skip_comments(tokens, p + 1);
                        if n < e && is_word(&tokens[n], "NULL") { saw_null = true; break; }
                    }
                }
                p += 1;
            }

            if !saw_null {
                let col_name = bare_name(&tokens[i]).to_string();
                out.push(finding(
                    "ddl.nullable_columns_should_be_explicit",
                    Severity::Info,
                    format!("Column `{}` does not declare NULL or NOT NULL — nullability falls back to session ANSI_NULL_DFLT_ON.", col_name),
                    Some(make_loc(&tokens[i])),
                    Some("Column nullability falls back to session `ANSI_NULL_DFLT_ON` and can differ between callers. Always specify `NULL` or `NOT NULL` explicitly in column definitions.".into()),
                ));
            }
        }
    }
    out
}

/// CREATE TABLE with no clustered index / PRIMARY KEY → a heap. Heaps fragment,
/// can't range-seek, and forward-pointer chase on update. Sometimes intentional
/// for staging/bulk-load, hence Info.
pub fn heap_table(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (open, close) in create_table_bodies(tokens) {
        // Scan the body for any signal of a clustered structure.
        let mut has_clustered_structure = false;
        let mut k = open;
        while k <= close {
            // PRIMARY KEY defaults to CLUSTERED; explicit CLUSTERED keyword also counts.
            if is_word(&tokens[k], "CLUSTERED") { has_clustered_structure = true; break; }
            if is_word(&tokens[k], "PRIMARY") {
                let n = skip_comments(tokens, k + 1);
                if n <= close && is_word(&tokens[n], "KEY") {
                    // PK is clustered unless explicitly NONCLUSTERED right after.
                    let m = skip_comments(tokens, n + 1);
                    let nonclustered = m <= close && is_word(&tokens[m], "NONCLUSTERED");
                    if !nonclustered { has_clustered_structure = true; break; }
                }
            }
            k += 1;
        }
        if !has_clustered_structure {
            // Anchor at the CREATE token preceding this body.
            let mut c = open;
            while c > 0 && !is_word(&tokens[c], "TABLE") { c -= 1; }
            out.push(finding(
                "hygiene.heap_table",
                Severity::Warning,
                "CREATE TABLE with no clustered index or PRIMARY KEY — this is a heap. Heaps fragment, can't be range-seeked, and accumulate forward pointers on updates.",
                Some(make_loc(&tokens[c])),
                Some("Add a clustered index (often the PRIMARY KEY). If a heap is intentional (short-lived staging / bulk load target), document it. For analytic, scan-heavy tables consider a clustered columnstore index instead.".into()),
            ));
        }
    }
    out
}

/// VARCHAR(MAX) / NVARCHAR(MAX) column declarations. MAX types are stored
/// off-row, can't be index keys, and amplify reads when the data is actually
/// small. Advisory — sometimes genuinely needed for large text/blobs.
/// SQL-Server scalar types that store large / off-row payloads. Including one of
/// these in a nonclustered index leaf is a heavy write-amplification commitment.
fn is_large_type_kw(kw: &str) -> bool {
    matches!(
        kw.to_ascii_lowercase().as_str(),
        "text" | "ntext" | "image" | "xml" | "sql_variant"
            | "geometry" | "geography" | "hierarchyid"
    )
}

/// `index.wide_covering_request` — flags a `CREATE ... INDEX ... INCLUDE (...)`
/// whose INCLUDE list is very wide, with an honest write-amplification caveat.
/// This is the source-side counterpart to the plan-derived caveat: it fires on a
/// covering index a developer (or our own plan-derived DDL) is about to deploy.
///
/// Heuristic: an INCLUDE list with more than `WIDE_INCLUDE` columns. Every INCLUDE
/// column is duplicated into the leaf, so a wide list slows every write and bloats
/// storage. We surface a trade-off, not a hard error — hence Info severity.
pub fn wide_covering_request(ctx: &RuleCtx) -> Vec<Finding> {
    const WIDE_INCLUDE: usize = 5;
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Locate each `CREATE [UNIQUE] [CLUSTERED|NONCLUSTERED] INDEX` and then find
    // its `INCLUDE ( ... )` clause (the parser already splits punctuation into
    // single-char tokens, so we count comma-separated items inside the parens).
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "CREATE") { continue; }
        // Confirm an INDEX keyword follows within a short window (allowing
        // UNIQUE / CLUSTERED / NONCLUSTERED qualifiers + comments in between).
        let mut k = skip_comments(tokens, i + 1);
        let mut saw_index = false;
        let mut steps = 0;
        while k < tokens.len() && steps < 5 {
            let w = &tokens[k];
            if w.kind == TokKind::Word {
                if name_eq_ci(bare_name(w), "INDEX") { saw_index = true; break; }
                // keep scanning across UNIQUE/CLUSTERED/NONCLUSTERED qualifiers
                if !(name_eq_ci(bare_name(w), "UNIQUE")
                    || name_eq_ci(bare_name(w), "CLUSTERED")
                    || name_eq_ci(bare_name(w), "NONCLUSTERED")) {
                    break;
                }
            } else if w.kind != TokKind::Comment {
                break;
            }
            k = skip_comments(tokens, k + 1);
            steps += 1;
        }
        if !saw_index { continue; }

        // From here, find the INCLUDE keyword before the next CREATE (statement
        // boundary heuristic). Then count the columns inside its parentheses.
        let mut j = skip_comments(tokens, k + 1);
        let mut include_at: Option<usize> = None;
        while j < tokens.len() {
            let w = &tokens[j];
            if w.kind == TokKind::Word {
                if name_eq_ci(bare_name(w), "CREATE") { break; } // next statement
                if name_eq_ci(bare_name(w), "INCLUDE") { include_at = Some(j); break; }
            }
            if w.kind == TokKind::Punct && w.text == ";" { break; }
            j += 1;
        }
        let Some(inc) = include_at else { continue; };

        // Expect `INCLUDE (` — find the open paren.
        let p = skip_comments(tokens, inc + 1);
        if !(p < tokens.len() && tokens[p].kind == TokKind::Punct && tokens[p].text == "(") {
            continue;
        }
        // Walk the parenthesised list, counting top-level columns + flagging any
        // large/LOB *type* keyword that appears (defensive: normal INCLUDE lists
        // are bare column names, but a malformed/typed list still gets caught).
        let mut depth = 0i32;
        let mut col_count = 0usize;
        let mut have_item = false;
        let mut large_types: Vec<String> = Vec::new();
        let mut q = p;
        while q < tokens.len() {
            let w = &tokens[q];
            if w.kind == TokKind::Punct {
                match w.text {
                    "(" => depth += 1,
                    ")" => { depth -= 1; if depth == 0 { if have_item { col_count += 1; } break; } }
                    "," if depth == 1 => { if have_item { col_count += 1; } have_item = false; }
                    _ => {}
                }
            } else if w.kind == TokKind::Word && depth >= 1 {
                have_item = true;
                if is_large_type_kw(bare_name(w)) {
                    let n = bare_name(w).to_string();
                    if !large_types.iter().any(|x| name_eq_ci(x, &n)) { large_types.push(n); }
                }
            }
            q += 1;
        }

        if col_count > WIDE_INCLUDE || !large_types.is_empty() {
            let reason = if !large_types.is_empty() {
                format!("its INCLUDE list carries large/LOB type(s) ({})", large_types.join(", "))
            } else {
                format!("its INCLUDE list has {} columns", col_count)
            };
            out.push(finding(
                "index.wide_covering_request",
                Severity::Info,
                format!("Wide covering index: {} — every INCLUDE column is copied into the nonclustered index leaf, so the index grows and each INSERT/UPDATE/DELETE that touches those columns pays to maintain the copy.", reason),
                Some(make_loc(&tokens[inc])),
                Some("Keep INCLUDE to the columns the covering query actually returns. Drop wide or LOB columns from INCLUDE if the lookup they remove is rare, and weigh the read win against the added write + storage cost before deploying — this is a trade-off, not a free win.".into()),
            ));
        }
    }
    out
}

/// Is the token before this type name a `@variable` (a DECLARE or a procedure
/// parameter) rather than a column name?
fn declares_a_variable(tokens: &[Token<'_>], type_idx: usize) -> bool {
    let Some(prev) = type_idx.checked_sub(1).and_then(|k| tokens.get(k)) else {
        return false;
    };
    if prev.text.starts_with('@') {
        return true;
    }
    // `DECLARE @x AS nvarchar(max)` puts AS between the two.
    if is_word(prev, "AS") {
        return type_idx
            .checked_sub(2)
            .and_then(|k| tokens.get(k))
            .map(|p| p.text.starts_with('@'))
            .unwrap_or(false);
    }
    false
}

pub fn varchar_max_overuse(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut count = 0;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word { continue; }
        let ty = t.text.to_ascii_uppercase();
        if ty != "VARCHAR" && ty != "NVARCHAR" { continue; }
        // pattern: <type> ( MAX )
        let lp = tokens.get(i + 1);
        let maxw = tokens.get(i + 2);
        let rp = tokens.get(i + 3);
        if lp.map(|p| p.text == "(").unwrap_or(false)
            && maxw.map(|m| m.kind == TokKind::Word && m.text.eq_ignore_ascii_case("MAX")).unwrap_or(false)
            && rp.map(|p| p.text == ")").unwrap_or(false)
        {
            // A local variable or parameter is not a column. `DECLARE @sql
            // nvarchar(max)` is the *required* type for sp_executesql's @stmt,
            // and column advice ("can't be index keys", "use NVARCHAR(400)")
            // is impossible to act on and wrong to follow.
            if declares_a_variable(tokens, i) { continue; }
            count += 1;
            if count > 3 { break; }
            out.push(finding(
                "ddl.varchar_max_overuse",
                Severity::Info,
                format!("{}(MAX) column: MAX types store off-row, can't be index keys, and amplify reads when the values are actually small.", ty),
                Some(make_loc(t)),
                Some("If the real data fits, use a bounded length (e.g. NVARCHAR(400)) so the column can be indexed and stays in-row. Reserve (MAX) for genuinely large text/blob content.".into()),
            ));
        }
    }
    out
}
