//! Reading a `.sql` file off disk without lying about what was in it.
//!
//! Two real-world hazards this handles, both discovered by linting files that
//! SSMS and Visual Studio write by default:
//!
//! * **UTF-16.** SSMS's historical default save encoding. With a BOM it is not
//!   valid UTF-8 and a naive read fails the whole run; *without* a BOM the NUL
//!   padding bytes are technically valid UTF-8, so a naive read "succeeds" and
//!   the file is silently analyzed as garbage — reported as clean.
//! * **UTF-8 BOM.** Valid UTF-8, but the 3 BOM bytes shift every column on
//!   line 1 by +3 unless they are stripped before analysis.

/// A source document that is ready to analyze.
pub struct Source {
    pub text: String,
    /// Set when the bytes were not plain UTF-8, for transparency in output.
    pub encoding_note: Option<&'static str>,
}

/// Decode a file's bytes into T-SQL text, tolerating the encodings SQL Server
/// tooling actually emits.
pub fn decode(bytes: &[u8]) -> Result<Source, String> {
    // UTF-16 with BOM.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return utf16(&bytes[2..], false).map(|text| Source {
            text,
            encoding_note: Some("decoded as UTF-16 LE"),
        });
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return utf16(&bytes[2..], true).map(|text| Source {
            text,
            encoding_note: Some("decoded as UTF-16 BE"),
        });
    }
    // UTF-8 with BOM: strip it so line-1 columns are not shifted by 3.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec())
            .map(|text| Source {
                text,
                encoding_note: None,
            })
            .map_err(|e| e.to_string());
    }
    // UTF-16 with no BOM: NUL padding is valid UTF-8, so this must be caught
    // by shape or the file is silently analyzed as nonsense and called clean.
    if let Some(big_endian) = sniff_bomless_utf16(bytes) {
        return utf16(bytes, big_endian).map(|text| Source {
            text,
            encoding_note: Some(if big_endian {
                "decoded as UTF-16 BE (no BOM)"
            } else {
                "decoded as UTF-16 LE (no BOM)"
            }),
        });
    }
    String::from_utf8(bytes.to_vec())
        .map(|text| Source {
            text,
            encoding_note: None,
        })
        .map_err(|e| e.to_string())
}

fn utf16(bytes: &[u8], big_endian: bool) -> Result<String, String> {
    if bytes.len() % 2 != 0 {
        return Err("UTF-16 input has an odd byte length".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|e| e.to_string())
}

/// Guess whether BOM-less bytes are UTF-16, and if so which endianness.
/// ASCII-dominant UTF-16 puts a NUL in every other byte; which half tells us
/// the byte order. Returns `Some(big_endian)`.
fn sniff_bomless_utf16(bytes: &[u8]) -> Option<bool> {
    if bytes.len() < 4 || bytes.len() % 2 != 0 {
        return None;
    }
    let sample = &bytes[..bytes.len().min(1024)];
    let (mut even_nul, mut odd_nul) = (0usize, 0usize);
    for (i, b) in sample.iter().enumerate() {
        if *b == 0 {
            if i % 2 == 0 {
                even_nul += 1;
            } else {
                odd_nul += 1;
            }
        }
    }
    let pairs = sample.len() / 2;
    // Require a strong majority so a stray NUL in a UTF-8 file is not enough.
    if odd_nul * 10 >= pairs * 8 && even_nul == 0 {
        return Some(false); // 'A' 0x00 -> little endian
    }
    if even_nul * 10 >= pairs * 8 && odd_nul == 0 {
        return Some(true); // 0x00 'A' -> big endian
    }
    None
}

/// Is this a SQL Server showplan document rather than T-SQL source?
///
/// This check has to run BEFORE [`looks_like_sql`], and that ordering is the
/// whole point. A `.sqlplan` embeds the query it describes in
/// `StatementText="SELECT …"`, so the keyword sniff happily calls it SQL, the
/// token rules find nothing lintable in XML, and the file is reported clean —
/// a false clean bill on a file that is full of real findings.
///
/// Matching is deliberately narrow: the `ShowPlanXML` root element, not merely
/// "looks like XML". A SQL file that happens to build an XML string must keep
/// being linted as SQL.
pub fn looks_like_plan_xml(text: &str) -> bool {
    // Showplan roots appear at the top of the document; scanning a prefix keeps
    // this O(1) on a large file and avoids matching the word inside a comment
    // halfway down a migration script.
    // Sliced by CHARS, not bytes: a byte slice can land mid-codepoint and panic
    // on a UTF-8/UTF-16-decoded file, and this runs on arbitrary user input.
    let head: String = text.chars().take(4096).collect::<String>().to_ascii_lowercase();
    head.contains("<showplanxml") || head.contains(":showplanxml")
}

/// Does this text contain anything that reads like a T-SQL statement?
///
/// A linter that reports "clean" on a truncated migration or a file of random
/// bytes is worse than one that says nothing, so a file with no recognizable
/// statement keyword is called out rather than counted as passing.
pub fn looks_like_sql(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP", "TRUNCATE",
        "WITH", "EXEC", "EXECUTE", "DECLARE", "SET", "GRANT", "REVOKE", "DENY", "BEGIN", "USE",
        "BACKUP", "RESTORE", "PRINT", "RAISERROR", "THROW", "WAITFOR", "GO", "IF", "WHILE",
        "VALUES", "FROM", "TABLE", "VIEW", "PROCEDURE", "FUNCTION", "INDEX", "TRIGGER",
    ];
    let stripped = strip_comments(text);
    let mut word = String::new();
    for ch in stripped.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphabetic() || ch == '_' {
            word.push(ch.to_ascii_uppercase());
        } else {
            if !word.is_empty() && KEYWORDS.contains(&word.as_str()) {
                return true;
            }
            word.clear();
        }
    }
    false
}

/// Is there any content here at all once comments and whitespace are removed?
pub fn is_effectively_empty(text: &str) -> bool {
    strip_comments(text).trim().is_empty()
}

/// Remove `--` line comments and `/* */` block comments. Deliberately simple:
/// this only feeds the two heuristics above, never the analyzer itself.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut block_depth = 0usize;
    while i < bytes.len() {
        if block_depth > 0 {
            if bytes[i] == '*' && bytes.get(i + 1) == Some(&'/') {
                block_depth -= 1;
                i += 2;
                continue;
            }
            if bytes[i] == '/' && bytes.get(i + 1) == Some(&'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if bytes[i] == '/' && bytes.get(i + 1) == Some(&'*') {
            block_depth = 1;
            i += 2;
            continue;
        }
        if bytes[i] == '-' && bytes.get(i + 1) == Some(&'-') {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str, bom: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if bom {
            v.extend_from_slice(&[0xFF, 0xFE]);
        }
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    #[test]
    fn utf16_with_bom_round_trips() {
        let src = decode(&utf16le("SELECT 1;\n", true)).unwrap();
        assert_eq!(src.text, "SELECT 1;\n");
        assert!(src.encoding_note.is_some());
    }

    #[test]
    fn utf16_without_bom_is_not_silently_garbage() {
        // The dangerous case: NUL padding is valid UTF-8, so a naive read
        // "succeeds" and the file is analyzed as nonsense.
        let raw = utf16le("SELECT * FROM dbo.Users WHERE UPPER(Email) = @e;\n", false);
        let src = decode(&raw).unwrap();
        assert!(src.text.starts_with("SELECT * FROM dbo.Users"));
        assert!(looks_like_sql(&src.text));
    }

    #[test]
    fn utf16_be_with_bom() {
        let mut v = vec![0xFE, 0xFF];
        for u in "SELECT 1;".encode_utf16() {
            v.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode(&v).unwrap().text, "SELECT 1;");
    }

    #[test]
    fn utf8_bom_is_stripped_so_columns_are_not_shifted() {
        let mut v = vec![0xEF, 0xBB, 0xBF];
        v.extend_from_slice(b"SELECT * FROM t;");
        let src = decode(&v).unwrap();
        assert_eq!(src.text, "SELECT * FROM t;");
        assert!(!src.text.starts_with('\u{feff}'));
    }

    #[test]
    fn plain_utf8_is_untouched() {
        let src = decode(b"SELECT 1; -- caf\xc3\xa9").unwrap();
        assert!(src.text.contains("café"));
        assert!(src.encoding_note.is_none());
    }

    #[test]
    fn garbage_is_not_mistaken_for_sql() {
        assert!(!looks_like_sql("SELEC FROMM dbo.Orders WHRE Id = ;;; (("));
        assert!(!looks_like_sql("\u{0}\u{1}\u{2}binary junk"));
        assert!(looks_like_sql("select 1"));
        assert!(looks_like_sql("/* header */\nEXEC dbo.Thing;"));
    }

    #[test]
    fn comment_only_and_empty_files() {
        assert!(is_effectively_empty(""));
        assert!(is_effectively_empty("   \n\t\n"));
        assert!(is_effectively_empty("-- just a note\n/* and a block */\n"));
        assert!(!is_effectively_empty("-- note\nSELECT 1;"));
    }

    #[test]
    fn keyword_match_is_word_bounded() {
        // "SELECTOR" must not count as SELECT.
        assert!(!looks_like_sql("SELECTOR ONLY"));
    }

    // ---- showplan detection ------------------------------------------------
    //
    // The bug these pin: a .sqlplan carries StatementText="SELECT …", so the
    // keyword sniff calls it SQL, the token rules find nothing in XML, and the
    // file is reported CLEAN. `dbopt lint plan.sqlplan` said 0 findings while
    // `dbopt plan.sqlplan` said 3, for the same file.

    #[test]
    fn showplan_is_detected_even_though_it_reads_as_sql() {
        let plan = r#"<?xml version="1.0"?>
<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan">
  <BatchSequence><Batch><Statements>
    <StmtSimple StatementText="SELECT * FROM dbo.Orders WHERE Id = 1">
  </Statements></Batch></BatchSequence></ShowPlanXML>"#;
        assert!(looks_like_plan_xml(plan));
        // It really does look like SQL to the keyword sniff — which is exactly
        // why the plan check has to run first.
        assert!(looks_like_sql(plan), "guard is only needed because this is true");
    }

    #[test]
    fn namespaced_showplan_root_is_detected() {
        let plan = r#"<sp:ShowPlanXML xmlns:sp="http://schemas.microsoft.com/sqlserver/2004/07/showplan"/>"#;
        assert!(looks_like_plan_xml(plan));
    }

    #[test]
    fn sql_that_merely_mentions_showplan_is_still_sql() {
        // A migration that builds XML, or turns showplan on, must keep being
        // linted as T-SQL. We match the ELEMENT, not the word.
        assert!(!looks_like_plan_xml("SET SHOWPLAN_XML ON;"));
        assert!(!looks_like_plan_xml(
            "SELECT 'ShowPlanXML' AS note FROM dbo.T;"
        ));
    }

    #[test]
    fn plan_sniff_does_not_panic_on_multibyte_input() {
        // The head is sliced by chars, not bytes: a byte slice at 4096 can land
        // mid-codepoint and panic, and this runs on arbitrary user files.
        let wide = "é".repeat(8000);
        assert!(!looks_like_plan_xml(&wide));
        let padded = format!("{}<ShowPlanXML>", "é".repeat(8000));
        assert!(!looks_like_plan_xml(&padded), "root element past the head is not a plan");
    }
}
