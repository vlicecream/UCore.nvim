use anyhow::Result;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use tree_sitter::Parser;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticItem {
    pub file_path: Option<String>,
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub severity: DiagnosticSeverity,
    pub source: &'static str,
    pub code: &'static str,
    pub message: String,
}

impl DiagnosticItem {
    fn new(
        file_path: Option<&str>,
        line: u32,
        character: u32,
        severity: DiagnosticSeverity,
        source: &'static str,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file_path: file_path.map(|path| path.replace('\\', "/")),
            line,
            character,
            end_line: line,
            end_character: character.saturating_add(1),
            severity,
            source,
            code,
            message: message.into(),
        }
    }
}

pub fn process_diagnostics(
    conn: &Connection,
    content: &str,
    file_path: Option<String>,
) -> Result<Value> {
    let mut items = Vec::new();
    items.extend(unreal_rule_diagnostics(content, file_path.as_deref())?);
    items.extend(include_diagnostics(conn, content, file_path.as_deref())?);
    Ok(json!({ "items": items }))
}

pub fn parse_build_diagnostics(output: &str) -> Value {
    json!({ "items": build_log_diagnostics(output) })
}

fn unreal_rule_diagnostics(content: &str, file_path: Option<&str>) -> Result<Vec<DiagnosticItem>> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;
    let _tree = parser.parse(content, None);

    let mut items = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();

        if starts_unreal_type_macro(trimmed) {
            let (macro_text, macro_end) = macro_invocation_text(&lines, index);

            if let Some((next_index, next_line)) = next_meaningful_line(&lines, macro_end + 1) {
                if !macro_matches_declaration(trimmed, next_line.trim_start()) {
                    items.push(DiagnosticItem::new(
                        file_path,
                        index as u32,
                        leading_spaces(line) as u32,
                        DiagnosticSeverity::Error,
                        "UCore",
                        "UHT001",
                        "Unreal reflection macro does not match the following declaration.",
                    ));
                }

                if !macro_text.starts_with("UENUM")
                    && !declaration_block_has_generated_body(&lines, next_index)
                {
                    items.push(DiagnosticItem::new(
                        file_path,
                        next_index as u32,
                        leading_spaces(next_line) as u32,
                        DiagnosticSeverity::Error,
                        "UCore",
                        "UHT002",
                        "Reflected type is missing GENERATED_BODY().",
                    ));
                }
            }
        }

        if trimmed.starts_with("UFUNCTION(") {
            let (macro_text, _) = macro_invocation_text(&lines, index);
            if macro_text.contains("BlueprintCallable") && !macro_text.contains("Category") {
                items.push(DiagnosticItem::new(
                    file_path,
                    index as u32,
                    leading_spaces(line) as u32,
                    DiagnosticSeverity::Hint,
                    "UCore",
                    "UEBP001",
                    "BlueprintCallable functions should declare a Category.",
                ));
            }
        }

        if trimmed.starts_with("UPROPERTY(")
        {
            let (macro_text, _) = macro_invocation_text(&lines, index);
            if macro_text.contains("BlueprintReadWrite")
                && !macro_text.contains("AllowPrivateAccess")
                && nearest_access_section(&lines, index) == Some("private")
            {
                items.push(DiagnosticItem::new(
                    file_path,
                    index as u32,
                    leading_spaces(line) as u32,
                    DiagnosticSeverity::Warning,
                    "UCore",
                    "UEBP002",
                    "Private BlueprintReadWrite property should use meta=(AllowPrivateAccess=true).",
                ));
            }
        }
    }

    Ok(items)
}

fn include_diagnostics(
    conn: &Connection,
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let mut items = Vec::new();

    for (line_index, line) in content.lines().enumerate() {
        let Some(include) = parse_include(line) else {
            continue;
        };

        let matches = include_matches(conn, &include)?;
        if matches.is_empty() {
            let character = line.find(&include).unwrap_or(0) as u32;
            items.push(DiagnosticItem::new(
                file_path,
                line_index as u32,
                character,
                DiagnosticSeverity::Warning,
                "UCore",
                "UEINC001",
                format!("Indexed header not found for include `{}`.", include),
            ));
            continue;
        }

        if file_path
            .map(|path| path.replace('\\', "/").contains("/Public/"))
            .unwrap_or(false)
            && matches.iter().any(|path| path.contains("/Private/"))
        {
            let character = line.find(&include).unwrap_or(0) as u32;
            items.push(DiagnosticItem::new(
                file_path,
                line_index as u32,
                character,
                DiagnosticSeverity::Warning,
                "UCore",
                "UEINC002",
                format!("Public header includes private header `{}`.", include),
            ));
        }
    }

    Ok(items)
}

fn build_log_diagnostics(output: &str) -> Vec<DiagnosticItem> {
    let msvc = Regex::new(
        r#"(?m)^(?P<file>[A-Za-z]:[^\r\n()]+)\((?P<line>\d+)(?:,(?P<col>\d+))?\):\s*(?P<level>fatal error|error|warning)\s*(?P<code>[A-Z]+\d+):\s*(?P<msg>.+)$"#,
    )
    .unwrap();
    let uht = Regex::new(
        r#"(?m)^(?P<file>[A-Za-z]:[^\r\n:]+):(?P<line>\d+):\s*(?P<level>Error|Warning):\s*(?P<msg>.+)$"#,
    )
    .unwrap();

    let mut items = Vec::new();

    for cap in msvc.captures_iter(output) {
        let level = cap.name("level").map(|m| m.as_str()).unwrap_or("error");
        let severity = if level.contains("warning") {
            DiagnosticSeverity::Warning
        } else {
            DiagnosticSeverity::Error
        };
        let line = cap
            .name("line")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let col = cap
            .name("col")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let code = cap.name("code").map(|m| m.as_str()).unwrap_or("MSVC");
        let msg = cap.name("msg").map(|m| m.as_str()).unwrap_or("");

        items.push(DiagnosticItem::new(
            cap.name("file").map(|m| m.as_str()),
            line,
            col,
            severity,
            "MSVC",
            "BUILD",
            format!("{}: {}", code, msg),
        ));
    }

    for cap in uht.captures_iter(output) {
        let level = cap.name("level").map(|m| m.as_str()).unwrap_or("Error");
        let severity = if level.eq_ignore_ascii_case("warning") {
            DiagnosticSeverity::Warning
        } else {
            DiagnosticSeverity::Error
        };
        let line = cap
            .name("line")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let msg = cap.name("msg").map(|m| m.as_str()).unwrap_or("");

        items.push(DiagnosticItem::new(
            cap.name("file").map(|m| m.as_str()),
            line,
            0,
            severity,
            "UHT",
            "BUILD",
            msg,
        ));
    }

    items
}

fn starts_unreal_type_macro(text: &str) -> bool {
    text.starts_with("UCLASS(")
        || text.starts_with("USTRUCT(")
        || text.starts_with("UENUM(")
        || text == "UCLASS"
        || text == "USTRUCT"
        || text == "UENUM"
}

fn macro_matches_declaration(macro_line: &str, declaration: &str) -> bool {
    if macro_line.starts_with("UCLASS") {
        declaration.contains("class ")
    } else if macro_line.starts_with("USTRUCT") {
        declaration.contains("struct ")
    } else if macro_line.starts_with("UENUM") {
        declaration.contains("enum ")
    } else {
        true
    }
}

fn macro_invocation_text(lines: &[&str], start: usize) -> (String, usize) {
    let mut text = String::new();
    let mut depth = 0i32;
    let end = (start + 8).min(lines.len());

    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line.trim());

        for ch in line.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }

        if depth <= 0 && text.contains('(') {
            return (text, index);
        }
    }

    (text, start)
}

fn declaration_block_has_generated_body(lines: &[&str], declaration_index: usize) -> bool {
    let end = (declaration_index + 20).min(lines.len());
    lines[declaration_index..end]
        .iter()
        .any(|line| line.contains("GENERATED_BODY") || line.contains("GENERATED_UCLASS_BODY"))
}

fn next_meaningful_line<'a>(lines: &'a [&str], start: usize) -> Option<(usize, &'a str)> {
    lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| {
            let text = line.trim();
            !text.is_empty() && !text.starts_with("//")
        })
        .map(|(index, line)| (index, *line))
}

fn nearest_access_section(lines: &[&str], line_index: usize) -> Option<&'static str> {
    for line in lines[..line_index.min(lines.len())].iter().rev().take(80) {
        match line.trim() {
            "public:" => return Some("public"),
            "protected:" => return Some("protected"),
            "private:" => return Some("private"),
            _ => {}
        }
    }

    Some("private")
}

fn parse_include(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("#include") {
        return None;
    }

    let start = trimmed.find(['"', '<'])?;
    let end_ch = if trimmed.as_bytes()[start] == b'<' { '>' } else { '"' };
    let rest = &trimmed[start + 1..];
    let end = rest.find(end_ch)?;
    Some(rest[..end].replace('\\', "/"))
}

fn include_matches(conn: &Connection, include: &str) -> Result<Vec<String>> {
    let filename = include.rsplit('/').next().unwrap_or(include);
    let pattern = format!("%/{}", include);
    let mut stmt = conn.prepare(
        r#"
        SELECT dp.full_path || '/' || sn.text
        FROM files f
        JOIN strings sn ON f.filename_id = sn.id
        JOIN dir_paths dp ON f.directory_id = dp.id
        WHERE f.is_header = 1
          AND (sn.text = ? OR dp.full_path || '/' || sn.text LIKE ?)
        ORDER BY length(dp.full_path || '/' || sn.text)
        LIMIT 20
        "#,
    )?;

    let rows = stmt.query_map(params![filename, pattern], |row| row.get::<_, String>(0))?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_string(conn: &Connection, text: &str) -> i64 {
        conn.execute("INSERT OR IGNORE INTO strings (text) VALUES (?)", [text])
            .unwrap();
        conn.query_row("SELECT id FROM strings WHERE text = ?", [text], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn insert_dir(conn: &Connection, parent_id: Option<i64>, name: &str) -> i64 {
        let name_id = insert_string(conn, name);
        conn.execute(
            "INSERT INTO directories (parent_id, name_id) VALUES (?, ?)",
            params![parent_id, name_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_header(conn: &Connection, directory_id: i64, name: &str) {
        let name_id = insert_string(conn, name);
        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?, ?, 'h', 1)",
            params![directory_id, name_id],
        )
        .unwrap();
    }

    #[test]
    fn detects_missing_generated_body() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            "UCLASS()\nclass AThing : public UObject {\n};\n",
            Some("C:/Project/AThing.h".to_string()),
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UHT002"));
    }

    #[test]
    fn parses_msvc_build_errors() {
        let value = parse_build_diagnostics(
            r#"C:\Project\Source\Game\Thing.cpp(12,34): error C2065: 'Foo': undeclared identifier"#,
        );
        let items = value["items"].as_array().unwrap();
        assert_eq!(items[0]["line"], 11);
        assert_eq!(items[0]["character"], 33);
        assert_eq!(items[0]["severity"], "error");
    }

    #[test]
    fn does_not_require_generated_body_for_uenum() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            "UENUM(BlueprintType)\nenum class EThing { One };\n",
            Some("C:/Project/EThing.h".to_string()),
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UHT002"));
    }

    #[test]
    fn detects_public_header_including_private_header() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let c = insert_dir(&conn, None, "C:");
        let project = insert_dir(&conn, Some(c), "Project");
        let source = insert_dir(&conn, Some(project), "Source");
        let game = insert_dir(&conn, Some(source), "Game");
        let private = insert_dir(&conn, Some(game), "Private");
        insert_header(&conn, private, "PrivateThing.h");

        let value = process_diagnostics(
            &conn,
            "#include \"PrivateThing.h\"\n",
            Some("C:/Project/Source/Game/Public/PublicThing.h".to_string()),
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UEINC002"));
    }
}
