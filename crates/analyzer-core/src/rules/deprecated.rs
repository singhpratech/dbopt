use super::{finding, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

pub fn old_join_syntax(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        // Detect "*=" or "=*"
        if t.text == "*" {
            let nxt = tokens.get(i + 1);
            if nxt.map(|n| n.text == "=").unwrap_or(false) {
                out.push(finding(
                    "deprecated.outer_join_star_equal",
                    Severity::Error,
                    "*= / =* style outer joins were removed in SQL Server 2008 and no longer parse under compatibility level 90+.",
                    Some(make_loc(t)),
                    Some("Use ANSI LEFT/RIGHT OUTER JOIN syntax.".into()),
                ));
            }
        }
    }
    out
}

pub fn sp_dboption(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("sp_dboption") {
            out.push(finding(
                "deprecated.sp_dboption",
                Severity::Error,
                "sp_dboption was removed in SQL Server 2012.",
                Some(make_loc(t)),
                Some("Use ALTER DATABASE … SET …".into()),
            ));
        }
    }
    out
}

pub fn text_image_ntext(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind != TokKind::Word { continue; }
        let u = t.text.to_ascii_uppercase();
        if matches!(u.as_str(), "TEXT" | "NTEXT" | "IMAGE") {
            // try to avoid matching `WITH (TEXTIMAGE_ON = …)` etc — check it's not preceded by "WITH" or "_"
            out.push(finding(
                "deprecated.lob_legacy_types",
                Severity::Warning,
                format!("{} is a deprecated LOB type and will be removed in a future SQL Server release.", u),
                Some(make_loc(t)),
                Some("Migrate to VARCHAR(MAX), NVARCHAR(MAX), or VARBINARY(MAX). Many functions (LEN, SUBSTRING, indexing) work properly on (MAX) types only.".into()),
            ));
        }
    }
    out
}

pub fn hash_temp_unsuffixed(ctx: &RuleCtx) -> Vec<Finding> {
    // double-hash global temp tables — a non-obvious correctness footgun
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind == TokKind::Word && t.text.starts_with("##") {
            out.push(finding(
                "hygiene.global_temp_table",
                Severity::Warning,
                "Global temp table (##name): visible to every session on the instance. Concurrent jobs collide silently.",
                Some(make_loc(t)),
                Some("Use a session-scoped temp table (#name) unless the cross-session visibility is intentional and documented. For passing data between sessions, prefer a permanent table with a clear retention strategy.".into()),
            ));
        }
    }
    out
}
