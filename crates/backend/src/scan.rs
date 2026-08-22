//! Database-level analysis endpoint.
//!
//! Pulls every user-defined programmable object from `sys.sql_modules`,
//! runs the static analyzer against each body, and returns a per-object
//! breakdown alongside an aggregate rule-incidence summary. The cost is
//! bounded by the number of modules (typically tens to a few hundred for
//! application databases); each analyze pass is pure CPU on the backend.

use axum::{extract::Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{routes::ConnectReq, sqlserver};

#[derive(Debug, Deserialize)]
pub struct ScanReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    #[serde(default = "default_version")]
    pub server_version: u16,
}

fn default_version() -> u16 { 2025 }

#[derive(Debug, Serialize)]
pub struct ScanObjectResult {
    pub schema_name: String,
    pub object_name: String,
    pub object_type: String,
    pub body_length: usize,
    pub findings_total: usize,
    pub findings_critical: usize,
    pub findings_error: usize,
    pub findings_warning: usize,
    pub findings_info: usize,
    pub top_rules: Vec<String>,
    /// Text-derived index suggestions withdrawn after checking the real
    /// catalog: the table already has an index with that leading key, or it
    /// is too small for an index to matter. Each note says which and why.
    #[serde(default)]
    pub catalog_notes: Vec<String>,
    #[serde(default)]
    pub suppressed: usize,
}

#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub server: String,
    pub database: Option<String>,
    pub objects_scanned: usize,
    pub findings_total: usize,
    pub findings_critical: usize,
    pub findings_error: usize,
    pub findings_warning: usize,
    pub findings_info: usize,
    pub rule_incidence: Vec<(String, usize)>,
    pub objects: Vec<ScanObjectResult>,
    pub duration_ms: u64,
    /// True when the live catalog (indexes + row counts) was consulted to
    /// check text-derived index advice. False = connection had no catalog
    /// access; the findings are then text-only, as the rule text says.
    #[serde(default)]
    pub catalog_consulted: bool,
    /// Findings withdrawn across all objects after the catalog check.
    #[serde(default)]
    pub suppressed_total: usize,
}

/// What the catalog check knows about one CREATE INDEX suggestion parsed out
/// of a finding's recommendation text.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCreateIndex {
    pub name: String,
    pub table: String,
    pub leading_key: String,
    pub keys: Vec<String>,
}

/// Parse `CREATE NONCLUSTERED INDEX [name] ON [s].[t] ([k1], [k2]) …` out of
/// rule text. Tolerates bracketed and bare identifiers and whitespace/newlines.
pub fn parse_create_index(text: &str) -> Option<ParsedCreateIndex> {
    let upper = text.to_ascii_uppercase();
    let start = upper.find("CREATE NONCLUSTERED INDEX")?;
    let rest = &text[start + "CREATE NONCLUSTERED INDEX".len()..];
    let on = rest.to_ascii_uppercase().find(" ON ")?;
    let name = strip_br(rest[..on].trim());
    let after_on = rest[on + 4..].trim_start();
    let paren = after_on.find('(')?;
    let table_raw = after_on[..paren].trim();
    let table = table_raw
        .rsplit('.')
        .next()
        .map(strip_br)
        .unwrap_or_default();
    let close = after_on[paren..].find(')')? + paren;
    let keys: Vec<String> = after_on[paren + 1..close]
        .split(',')
        .map(|k| strip_br(k.trim()))
        .filter(|k| !k.is_empty())
        .collect();
    let leading_key = keys.first().cloned()?;
    if name.is_empty() || table.is_empty() {
        return None;
    }
    Some(ParsedCreateIndex { name, table, leading_key, keys })
}

fn strip_br(s: &str) -> String {
    s.trim().trim_matches(|c| c == '[' || c == ']').to_string()
}

/// Catalog shape used to check text-derived index advice.
#[derive(Debug, Default)]
pub struct CatalogShape {
    /// (lower table) -> [(index name, leading key column lower, key columns)]
    pub indexes: HashMap<String, Vec<(String, String, Vec<String>)>>,
    /// (lower table) -> rows (heap/clustered partitions only)
    pub rows: HashMap<String, u64>,
}

impl CatalogShape {
    pub fn from_bundle(b: &analyzer_core::dmv::DmvBundle) -> Self {
        let mut shape = CatalogShape::default();
        for ix in &b.indexes {
            let Some(lead) = ix.key_columns.first() else { continue };
            shape
                .indexes
                .entry(ix.table_name.to_ascii_lowercase())
                .or_default()
                .push((ix.index_name.clone(), lead.to_ascii_lowercase(), ix.key_columns.clone()));
        }
        for (k, v) in analyzer_core::dmv::table_row_counts(&b.partition_stats) {
            let table = k.rsplit('.').next().unwrap_or(&k).to_string();
            shape.rows.insert(table, v);
        }
        shape
    }

    /// `Some(note)` when the suggestion should be withdrawn, with the reason.
    pub fn check(&self, rule: &str, p: &ParsedCreateIndex) -> Option<String> {
        let t = p.table.to_ascii_lowercase();
        if let Some(rows) = self.rows.get(&t) {
            if *rows < analyzer_core::dmv::SMALL_TABLE_ROWS {
                return Some(format!(
                    "{rule}: withdrew index suggestion on {} — the table holds {rows} row(s) (one page); an index cannot reduce its cost.",
                    p.table
                ));
            }
        }
        if let Some(list) = self.indexes.get(&t) {
            if let Some((name, _, keys)) = list.iter().find(|(_, lead, _)| *lead == p.leading_key.to_ascii_lowercase()) {
                return Some(format!(
                    "{rule}: withdrew CREATE INDEX [{}] on {} — the catalog already has {} leading with {} (key: {}). If it lacks INCLUDE columns the query needs, widen it WITH (DROP_EXISTING = ON) rather than adding a second index on the same key.",
                    p.name, p.table, name, p.leading_key, keys.join(", ")
                ));
            }
            if let Some((name, _, keys)) = list.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(&p.name)) {
                return Some(format!(
                    "{rule}: withdrew CREATE INDEX [{}] on {} — that name already exists on the table (key: {}) with a different key; the statement would fail. Pick a distinct name or widen {} WITH (DROP_EXISTING = ON) if it should carry this key.",
                    p.name, p.table, keys.join(", "), name
                ));
            }
        }
        None
    }
}

pub async fn scan_database(Json(req): Json<ScanReq>) -> impl IntoResponse {
    let started = std::time::Instant::now();
    let modules = match sqlserver::enumerate_modules(&req.conn).await {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    // Real catalog (indexes + row counts), so a text-derived "no matching
    // index is declared in this batch" is checked against what exists. Best
    // effort: a login without catalog access gets text-only findings.
    let catalog = match sqlserver::pull_catalog_shape(&req.conn).await {
        Ok(b) => Some(CatalogShape::from_bundle(&b)),
        Err(e) => {
            tracing::warn!(target: "scan", "catalog shape unavailable ({e}); index advice is text-only");
            None
        }
    };

    let mut total_c = 0usize;
    let mut total_e = 0usize;
    let mut total_w = 0usize;
    let mut total_i = 0usize;
    let mut suppressed_total = 0usize;
    let mut rule_hits: HashMap<String, usize> = HashMap::new();
    let mut objects: Vec<ScanObjectResult> = Vec::with_capacity(modules.len());

    for m in modules {
        let input = analyzer_core::AnalyzeInput {
            sql: Some(m.body.clone()),
            plan_xml: None,
            dmv_bundle: None,
            server_version: Some(req.server_version),
            engine: None, // SQL Server (v0.x default)
        };
        let mut report = analyzer_core::analyze(&input);
        let mut catalog_notes: Vec<String> = Vec::new();
        if let Some(cat) = &catalog {
            report.findings.retain(|f| {
                let Some(rec) = f.recommendation.as_deref() else { return true };
                if !f.rule.0.starts_with("index.") { return true; }
                let Some(parsed) = parse_create_index(rec) else { return true };
                match cat.check(&f.rule.0, &parsed) {
                    Some(note) => { catalog_notes.push(note); false }
                    None => true,
                }
            });
        }
        let suppressed = catalog_notes.len();
        suppressed_total += suppressed;
        let mut c = 0usize;
        let mut e = 0usize;
        let mut w = 0usize;
        let mut i = 0usize;
        let mut local_rules: HashMap<String, usize> = HashMap::new();
        for f in &report.findings {
            match f.severity {
                analyzer_core::findings::Severity::Critical => c += 1,
                analyzer_core::findings::Severity::Error => e += 1,
                analyzer_core::findings::Severity::Warning => w += 1,
                analyzer_core::findings::Severity::Info => i += 1,
            }
            *local_rules.entry(f.rule.0.clone()).or_insert(0) += 1;
            *rule_hits.entry(f.rule.0.clone()).or_insert(0) += 1;
        }
        let mut sorted_local: Vec<(String, usize)> = local_rules.into_iter().collect();
        sorted_local.sort_by(|a, b| b.1.cmp(&a.1));
        let top_rules = sorted_local.into_iter().take(5).map(|(k, _)| k).collect();

        total_c += c; total_e += e; total_w += w; total_i += i;
        objects.push(ScanObjectResult {
            schema_name: m.schema_name,
            object_name: m.object_name,
            object_type: m.object_type,
            body_length: m.body.len(),
            findings_total: c + e + w + i,
            findings_critical: c,
            findings_error: e,
            findings_warning: w,
            findings_info: i,
            top_rules,
            catalog_notes,
            suppressed,
        });
    }

    // Sort objects: most-painful first.
    objects.sort_by(|a, b| {
        b.findings_critical.cmp(&a.findings_critical)
            .then(b.findings_error.cmp(&a.findings_error))
            .then(b.findings_warning.cmp(&a.findings_warning))
            .then(b.findings_total.cmp(&a.findings_total))
    });

    let mut rule_incidence: Vec<(String, usize)> = rule_hits.into_iter().collect();
    rule_incidence.sort_by(|a, b| b.1.cmp(&a.1));

    let result = ScanResult {
        server: req.conn.server.clone(),
        database: req.conn.database.clone(),
        objects_scanned: objects.len(),
        findings_total: total_c + total_e + total_w + total_i,
        findings_critical: total_c,
        findings_error: total_e,
        findings_warning: total_w,
        findings_info: total_i,
        rule_incidence,
        objects,
        duration_ms: started.elapsed().as_millis() as u64,
        catalog_consulted: catalog.is_some(),
        suppressed_total,
    };
    (StatusCode::OK, Json(result)).into_response()
}

#[cfg(test)]
mod catalog_check_tests {
    use super::*;
    use analyzer_core::dmv::{DmvBundle, IndexMeta, PartitionStats};

    fn bundle() -> DmvBundle {
        DmvBundle {
            indexes: vec![
                IndexMeta { schema_name: "dbo".into(), table_name: "Orders".into(), index_name: "IX_Orders_Status".into(), key_columns: vec!["Status".into()], ..Default::default() },
                IndexMeta { schema_name: "dbo".into(), table_name: "Shipments".into(), index_name: "IX_Shipments_TrackingCode".into(), key_columns: vec!["TrackingCode".into()], ..Default::default() },
            ],
            partition_stats: vec![
                PartitionStats { schema_name: "dbo".into(), table_name: "Categories".into(), index_name: None, row_count: 20, index_id: Some(0), ..Default::default() },
                PartitionStats { schema_name: "dbo".into(), table_name: "Orders".into(), index_name: Some("PK_Orders".into()), row_count: 1_000_000, index_id: Some(1), ..Default::default() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn parses_rule_ddl_with_brackets_and_newlines() {
        let p = parse_create_index("Add a covering index:\n\nCREATE NONCLUSTERED INDEX [IX_Orders_Status]\n  ON dbo.Orders ([Status])\n  INCLUDE ([OrderID]);\nmore text").unwrap();
        assert_eq!(p.name, "IX_Orders_Status");
        assert_eq!(p.table, "Orders");
        assert_eq!(p.leading_key, "Status");
        let p = parse_create_index("CREATE NONCLUSTERED INDEX [IX_X_A_B]\n    ON [dbo].[X] ([A], [B]) INCLUDE ([C]);").unwrap();
        assert_eq!(p.keys, vec!["A", "B"]);
        assert!(parse_create_index("no ddl here").is_none());
    }

    #[test]
    fn existing_leading_key_withdraws_the_suggestion() {
        let cat = CatalogShape::from_bundle(&bundle());
        let p = parse_create_index("CREATE NONCLUSTERED INDEX [IX_Shipments_TrackingCode] ON dbo.Shipments ([TrackingCode]) INCLUDE ([ShipmentID]);").unwrap();
        let note = cat.check("index.missing_index_from_predicate", &p).unwrap();
        assert!(note.contains("already has IX_Shipments_TrackingCode leading with TrackingCode"), "{note}");
        assert!(note.contains("DROP_EXISTING = ON"));
    }

    #[test]
    fn tiny_table_withdraws_the_suggestion() {
        let cat = CatalogShape::from_bundle(&bundle());
        let p = parse_create_index("CREATE NONCLUSTERED INDEX [IX_Categories_Name] ON dbo.Categories ([Name]) INCLUDE ([CategoryID]);").unwrap();
        let note = cat.check("index.missing_index_from_predicate", &p).unwrap();
        assert!(note.contains("20 row(s)"), "{note}");
    }

    #[test]
    fn a_genuinely_missing_index_on_a_big_table_survives() {
        let cat = CatalogShape::from_bundle(&bundle());
        let p = parse_create_index("CREATE NONCLUSTERED INDEX [IX_Orders_CustomerID] ON dbo.Orders ([CustomerID]) INCLUDE ([TotalAmount]);").unwrap();
        assert!(cat.check("index.missing_index_from_predicate", &p).is_none());
    }

    #[test]
    fn name_collision_with_a_different_key_is_called_out() {
        let cat = CatalogShape::from_bundle(&bundle());
        let p = parse_create_index("CREATE NONCLUSTERED INDEX [IX_Orders_Status] ON dbo.Orders ([Channel], [OrderDate]);").unwrap();
        let note = cat.check("index.missing_index_from_predicate", &p).unwrap();
        assert!(note.contains("that name already exists"), "{note}");
    }
}
