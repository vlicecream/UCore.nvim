use anyhow::Result;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;
use tree_sitter::Parser;
use tracing::info;

use crate::types::OpenBufferOverlay;

const PROJECT_TEXT_VISIBILITY_SCAN_LIMIT: usize = 64;
const ENGINE_TEXT_VISIBILITY_SCAN_LIMIT: usize = 64;

static DIAGNOSTICS_LOG_ENABLED: OnceLock<bool> = OnceLock::new();

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

    fn with_end(mut self, end_line: u32, end_character: u32) -> Self {
        self.end_line = end_line;
        self.end_character = end_character.max(self.character.saturating_add(1));
        self
    }
}

pub fn process_diagnostics(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    content: &str,
    file_path: Option<String>,
    open_files: &[OpenBufferOverlay],
) -> Result<Value> {
    let mut items = Vec::new();
    let log_enabled = diagnostics_log_enabled();
    let file_label = file_path.as_deref().unwrap_or("-");

    extend_diagnostic_phase(
        &mut items,
        "unreal_rules",
        file_label,
        log_enabled,
        || unreal_rule_diagnostics(content, file_path.as_deref()),
    )?;
    extend_diagnostic_phase(
        &mut items,
        "include_rules",
        file_label,
        log_enabled,
        || unreal_include_diagnostics(conn, content, file_path.as_deref()),
    )?;
    extend_diagnostic_phase(
        &mut items,
        "missing_return",
        file_label,
        log_enabled,
        || missing_return_diagnostics(content, file_path.as_deref()),
    )?;
    extend_diagnostic_phase(
        &mut items,
        "override_rules",
        file_label,
        log_enabled,
        || override_diagnostics(conn, engine_conn, content, file_path.as_deref()),
    )?;
    extend_diagnostic_phase(
        &mut items,
        "visible_types",
        file_label,
        log_enabled,
        || {
            missing_visible_type_diagnostics(
                conn,
                engine_conn,
                content,
                file_path.as_deref(),
            )
        },
    )?;
    extend_diagnostic_phase(
        &mut items,
        "incomplete_member_decl",
        file_label,
        log_enabled,
        || incomplete_member_declaration_diagnostics(content, file_path.as_deref()),
    )?;
    extend_diagnostic_phase(
        &mut items,
        "missing_impl",
        file_label,
        log_enabled,
        || missing_implementation_diagnostics(conn, content, file_path.as_deref(), open_files),
    )?;
    Ok(json!({ "items": items }))
}

fn diagnostics_log_enabled() -> bool {
    *DIAGNOSTICS_LOG_ENABLED.get_or_init(|| {
        std::env::var("UCORE_QUERY_LOG")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false)
    })
}

fn extend_diagnostic_phase<F>(
    items: &mut Vec<DiagnosticItem>,
    phase_name: &'static str,
    file_label: &str,
    log_enabled: bool,
    build: F,
) -> Result<()>
where
    F: FnOnce() -> Result<Vec<DiagnosticItem>>,
{
    let started_at = Instant::now();
    let phase_items = build()?;

    if log_enabled {
        info!(
            target: "ucore::diagnostics",
            "Diagnostics phase file={} phase={} ms={} items={}",
            file_label,
            phase_name,
            started_at.elapsed().as_millis(),
            phase_items.len()
        );
    }

    items.extend(phase_items);
    Ok(())
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
                    )
                    .with_end(index as u32, line.len() as u32));
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
                    )
                    .with_end(next_index as u32, next_line.len() as u32));
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
                )
                .with_end(index as u32, line.len() as u32));
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
                )
                .with_end(index as u32, line.len() as u32));
            }
        }
    }

    Ok(items)
}

fn unreal_include_diagnostics(
    conn: &Connection,
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    if is_header_file(file_path) {
        items.extend(generated_header_order_diagnostics(content, file_path));
    }
    if is_source_file(file_path) {
        items.extend(source_first_include_diagnostics(conn, content, file_path)?);
    }
    Ok(items)
}

fn missing_return_diagnostics(
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };

    if !is_cpp_source_or_header(file_path) {
        return Ok(Vec::new());
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    collect_missing_return_items(tree.root_node(), content, file_path, &mut items);
    Ok(items)
}

fn override_diagnostics(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };

    if !is_cpp_source_or_header(file_path) {
        return Ok(Vec::new());
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    collect_override_items(
        conn,
        engine_conn,
        tree.root_node(),
        content,
        file_path,
        &mut items,
    )?;
    Ok(items)
}

fn missing_visible_type_diagnostics(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };

    if !is_cpp_source_or_header(file_path) {
        return Ok(Vec::new());
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };

    let root = tree.root_node();
    let local_types = collect_local_type_visibility(root, content);
    let include_paths = collect_include_paths(content);
    let project_visible_file_ids = reachable_project_file_ids(conn, file_path, &include_paths);
    let engine_seed_include_paths =
        collect_engine_seed_include_paths(conn, &project_visible_file_ids, &include_paths);
    let engine_visible_file_ids = engine_conn
        .map(|engine_conn| reachable_engine_file_ids(engine_conn, &engine_seed_include_paths))
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut project_lookup_cache = HashMap::new();
    let mut engine_lookup_cache = HashMap::new();
    let mut project_text_visibility_cache = HashMap::new();
    let mut engine_text_visibility_cache = HashMap::new();
    let mut project_text_forward_decl_cache = HashMap::new();
    let mut engine_text_forward_decl_cache = HashMap::new();
    let mut items = Vec::new();

    collect_missing_visible_type_items(
        conn,
        engine_conn,
        root,
        content,
        file_path,
        &local_types,
        &include_paths,
        &engine_seed_include_paths,
        &project_visible_file_ids,
        &engine_visible_file_ids,
        &mut project_lookup_cache,
        &mut engine_lookup_cache,
        &mut project_text_visibility_cache,
        &mut engine_text_visibility_cache,
        &mut project_text_forward_decl_cache,
        &mut engine_text_forward_decl_cache,
        &mut seen,
        &mut items,
    )?;

    Ok(items)
}

fn collect_missing_visible_type_items(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    local_types: &LocalTypeVisibility,
    include_paths: &HashSet<String>,
    engine_seed_include_paths: &HashSet<String>,
    project_visible_file_ids: &HashSet<i64>,
    engine_visible_file_ids: &HashSet<i64>,
    project_lookup_cache: &mut HashMap<String, TypeVisibilityMatch>,
    engine_lookup_cache: &mut HashMap<String, TypeVisibilityMatch>,
    project_text_visibility_cache: &mut HashMap<String, bool>,
    engine_text_visibility_cache: &mut HashMap<String, bool>,
    project_text_forward_decl_cache: &mut HashMap<String, bool>,
    engine_text_forward_decl_cache: &mut HashMap<String, bool>,
    seen: &mut HashSet<(u32, u32, String)>,
    items: &mut Vec<DiagnosticItem>,
) -> Result<()> {
    for reference in type_references_for_node(node, content) {
        let key = (reference.line, reference.character, reference.name.clone());
        if !seen.insert(key) {
            continue;
        }

        if local_types.defined.contains(&reference.name) {
            continue;
        }

        if reference.requires_complete
            && local_types.forward_declared.contains(&reference.name)
            && !local_types.defined.contains(&reference.name)
        {
            items.push(
                DiagnosticItem::new(
                    Some(file_path),
                    reference.line,
                    reference.character,
                    DiagnosticSeverity::Error,
                    "UCore",
                    "UECPP005",
                    format!(
                        "Type {} is only forward-declared here and cannot be used by value or as a base class.",
                        reference.name
                    ),
                )
                .with_end(reference.end_line, reference.end_character),
            );
            continue;
        }

        if !reference.requires_complete
            && local_types.forward_declared.contains(&reference.name)
            && !local_types.defined.contains(&reference.name)
        {
            continue;
        }

        if is_unreal_implicitly_visible_type(&reference.name) {
            continue;
        }

        let project_match = cached_type_visibility_lookup(
            project_lookup_cache,
            conn,
            &reference.name,
            Some(project_visible_file_ids),
            Some(include_paths),
        )?;

        let project_visible = project_match.visible
            || (project_match.exists
                && visible_type_declared_in_files(
                    conn,
                    &reference.name,
                    project_visible_file_ids,
                    project_text_visibility_cache,
                    PROJECT_TEXT_VISIBILITY_SCAN_LIMIT,
                )?);

        let project_forward_declared = !reference.requires_complete
            && visible_type_forward_declared_in_files(
                conn,
                &reference.name,
                project_visible_file_ids,
                project_text_forward_decl_cache,
                PROJECT_TEXT_VISIBILITY_SCAN_LIMIT,
            )?;

        if project_visible || project_forward_declared {
            continue;
        }

        let engine_match = if let Some(engine_conn) = engine_conn {
            cached_type_visibility_lookup(
                engine_lookup_cache,
                engine_conn,
                &reference.name,
                Some(engine_visible_file_ids),
                Some(engine_seed_include_paths),
            )?
        } else {
            TypeVisibilityMatch::default()
        };

        let engine_visible = if let Some(engine_conn) = engine_conn {
            engine_match.visible
                || (engine_match.exists
                    && visible_type_declared_in_files(
                        engine_conn,
                        &reference.name,
                        engine_visible_file_ids,
                        engine_text_visibility_cache,
                        ENGINE_TEXT_VISIBILITY_SCAN_LIMIT,
                    )?)
        } else {
            false
        };

        let engine_forward_declared = if let Some(engine_conn) = engine_conn {
            !reference.requires_complete
                && visible_type_forward_declared_in_files(
                    engine_conn,
                    &reference.name,
                    engine_visible_file_ids,
                    engine_text_forward_decl_cache,
                    ENGINE_TEXT_VISIBILITY_SCAN_LIMIT,
                )?
        } else {
            false
        };

        if engine_visible || engine_forward_declared {
            continue;
        }

        if project_match.exists || engine_match.exists {
            items.push(
                DiagnosticItem::new(
                    Some(file_path),
                    reference.line,
                    reference.character,
                    DiagnosticSeverity::Error,
                    "UCore",
                    "UECPP004",
                    format!(
                        "Type {} is not visible here. Missing include or forward declaration.",
                        reference.name
                    ),
                )
                .with_end(reference.end_line, reference.end_character),
            );
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_missing_visible_type_items(
            conn,
            engine_conn,
            child,
            content,
            file_path,
            local_types,
            include_paths,
            engine_seed_include_paths,
            project_visible_file_ids,
            engine_visible_file_ids,
            project_lookup_cache,
            engine_lookup_cache,
            project_text_visibility_cache,
            engine_text_visibility_cache,
            project_text_forward_decl_cache,
            engine_text_forward_decl_cache,
            seen,
            items,
        )?;
    }

    Ok(())
}

#[derive(Clone, Debug, Default)]
struct TypeVisibilityMatch {
    exists: bool,
    visible: bool,
}

#[derive(Clone, Debug)]
struct TypeReference {
    name: String,
    line: u32,
    character: u32,
    end_line: u32,
    end_character: u32,
    requires_complete: bool,
}

fn collect_missing_return_items(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    items: &mut Vec<DiagnosticItem>,
) {
    if let Some(item) = missing_return_item(node, content, file_path) {
        items.push(item);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_missing_return_items(child, content, file_path, items);
    }
}

fn missing_return_item(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
) -> Option<DiagnosticItem> {
    if !matches!(node.kind(), "function_definition" | "unreal_function_definition") {
        return None;
    }

    let declarator = find_child_by_field(node, "declarator")?;
    let name_node = find_name_node(declarator)?;
    let name = node_text(name_node, content).trim().to_string();
    if name.is_empty() {
        return None;
    }

    let return_type = find_child_by_field(node, "type")
        .map(|type_node| node_text(type_node, content).to_string())
        .unwrap_or_else(|| extract_prefix_type(node, Some(declarator), content));
    let normalized_return = normalize_space(&return_type);

    if normalized_return.is_empty()
        || normalized_return == "void"
        || normalized_return == "auto"
        || normalized_return.ends_with("decltype(auto)")
    {
        return None;
    }

    let body = find_child_by_type(node, "compound_statement")?;
    if block_guarantees_return(body) {
        return None;
    }

    let start = name_node.start_position();
    let end = name_node.end_position();
    Some(
        DiagnosticItem::new(
            Some(file_path),
            start.row as u32,
            start.column as u32,
            DiagnosticSeverity::Error,
            "UCore",
            "UECPP003",
            format!(
                "Non-void function {} can reach the end without returning a value.",
                name
            ),
        )
        .with_end(end.row as u32, end.column as u32),
    )
}

fn block_guarantees_return(node: tree_sitter::Node) -> bool {
    match node.kind() {
        "return_statement" => return true,
        "compound_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if statement_guarantees_return(child) {
                    return true;
                }
            }
            return false;
        }
        _ => {}
    }

    statement_guarantees_return(node)
}

fn statement_guarantees_return(node: tree_sitter::Node) -> bool {
    match node.kind() {
        "return_statement" => true,
        "compound_statement" => block_guarantees_return(node),
        "else_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if statement_guarantees_return(child) {
                    return true;
                }
            }
            false
        }
        "if_statement" => {
            let Some(consequence) = node.child_by_field_name("consequence") else {
                return false;
            };
            let Some(alternative) = node.child_by_field_name("alternative") else {
                return false;
            };
            statement_guarantees_return(consequence) && statement_guarantees_return(alternative)
        }
        _ => false,
    }
}

fn missing_implementation_diagnostics(
    conn: &Connection,
    content: &str,
    file_path: Option<&str>,
    open_files: &[OpenBufferOverlay],
) -> Result<Vec<DiagnosticItem>> {
    let Some(header_path) = file_path else {
        return Ok(Vec::new());
    };

    if !is_header_file(header_path) {
        return Ok(Vec::new());
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };

    let root = tree.root_node();
    let overlay_texts = open_file_texts(open_files);
    let source_candidates = header_to_source_candidates(header_path);
    let source_texts = source_candidates
        .iter()
        .filter_map(|path| {
            let normalized_path = path.replace('\\', "/");
            overlay_texts
                .get(&normalized_path)
                .cloned()
                .or_else(|| fs::read_to_string(path).ok().map(|text| normalize_space(&text)))
                .map(|text| (normalized_path, text))
        })
        .collect::<Vec<_>>();

    let mut items = Vec::new();
    collect_missing_impl_items(
        conn,
        root,
        content,
        header_path,
        &source_candidates,
        &source_texts,
        &mut items,
    )?;
    Ok(items)
}

fn open_file_texts(open_files: &[OpenBufferOverlay]) -> HashMap<String, String> {
    let mut texts = HashMap::new();

    for item in open_files {
        let path = item.file_path.replace('\\', "/");
        if path.is_empty() {
            continue;
        }

        texts.insert(path, normalize_space(&item.content));
    }

    texts
}

fn incomplete_member_declaration_diagnostics(
    content: &str,
    file_path: Option<&str>,
) -> Result<Vec<DiagnosticItem>> {
    let Some(header_path) = file_path else {
        return Ok(Vec::new());
    };

    if !is_header_file(header_path) {
        return Ok(Vec::new());
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };

    let root = tree.root_node();
    let mut items = Vec::new();
    collect_incomplete_member_decl_items(root, content, header_path, &mut items);
    Ok(items)
}

fn collect_incomplete_member_decl_items(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    items: &mut Vec<DiagnosticItem>,
) {
    if let Some(item) = incomplete_member_declaration_item(node, content, file_path) {
        items.push(item);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_incomplete_member_decl_items(child, content, file_path, items);
    }
}

fn incomplete_member_declaration_item(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
) -> Option<DiagnosticItem> {
    if !matches!(node.kind(), "field_declaration" | "ERROR") {
        return None;
    }

    let text = node_text(node, content).trim().to_string();
    if text.is_empty()
        || text.contains(';')
        || text.contains('{')
        || !text.contains('(')
        || !text.contains(')')
    {
        return None;
    }

    let Some(_class_name) = find_enclosing_class_name(node, content) else {
        return None;
    };

    let declarator = find_child_by_field(node, "declarator");
    let name_node = declarator
        .and_then(find_name_node)
        .or_else(|| find_name_node(node));
    let name = name_node
        .map(|name_node| node_text(name_node, content).trim().to_string())
        .or_else(|| {
            let regex = Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*\(").ok()?;
            regex
                .captures_iter(&text)
                .last()
                .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        })
        .unwrap_or_default();

    if name.is_empty() {
        return None;
    }

    let start = declarator
        .map(|declarator| declaration_start(node, declarator))
        .unwrap_or_else(|| node.start_position());
    let end = name_node
        .map(|name_node| name_node.end_position())
        .unwrap_or_else(|| node.end_position());

    Some(
        DiagnosticItem::new(
            Some(file_path),
            start.row as u32,
            start.column as u32,
            DiagnosticSeverity::Error,
            "UCore",
            "UECPP002",
            format!("Incomplete member function declaration for {}. Expected ';' or a function body.", name),
        )
        .with_end(end.row as u32, end.column as u32),
    )
}

fn collect_override_items(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    items: &mut Vec<DiagnosticItem>,
) -> Result<()> {
    if let Some(decl) = member_function_declaration(node, content, file_path) {
        if contains_token(&decl.full_text, "override") {
            let buffer_base_names = enclosing_class_base_names(node, content);
            let has_match = has_base_member_named_across_dbs(
                conn,
                engine_conn,
                &decl.class_name,
                &decl.name,
                &buffer_base_names,
            )?;

            if !has_match {
                items.push(
                    DiagnosticItem::new(
                        decl.file_path.as_deref(),
                        decl.line,
                        decl.character,
                        DiagnosticSeverity::Error,
                        "UCore",
                        "UECPP007",
                        format!(
                            "Function {}::{} is marked override but no base member with the same name was found.",
                            decl.class_name, decl.name
                        ),
                    )
                    .with_end(decl.end_line, decl.end_character),
                );
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_override_items(conn, engine_conn, child, content, file_path, items)?;
    }

    Ok(())
}

fn collect_missing_impl_items(
    conn: &Connection,
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    source_candidates: &[String],
    source_texts: &[(String, String)],
    items: &mut Vec<DiagnosticItem>,
) -> Result<()> {
    if let Some(decl) = member_function_declaration(node, content, file_path) {
        let target = source_texts
            .first()
            .map(|(path, _)| path.clone())
            .or_else(|| source_candidates.first().cloned())
            .unwrap_or_else(|| expected_source_path(&decl.class_name));

        for expected in &decl.expected_definitions {
            let definition_signature = build_definition_signature(&decl, expected);
            let found_in_text = source_texts
                .iter()
                .any(|(_, text)| has_definition_text(text, &definition_signature));
            let found = if found_in_text {
                true
            } else {
                has_indexed_definition(conn, &decl, expected)?
            };

            if !found {
                items.push(
                    DiagnosticItem::new(
                        decl.file_path.as_deref(),
                        decl.line,
                        decl.character,
                        DiagnosticSeverity::Warning,
                        "UCore",
                        "UECPP001",
                        format!(
                            "No matching .cpp {} found for {}::{}{}. Expected in {}.",
                            expected.message_label,
                            decl.class_name,
                            expected.name,
                            decl.parameters,
                            target
                        ),
                    )
                    .with_end(decl.end_line, decl.end_character),
                );
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_missing_impl_items(
            conn,
            child,
            content,
            file_path,
            source_candidates,
            source_texts,
            items,
        )?;
    }

    Ok(())
}

fn has_base_member_named_across_dbs(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    class_name: &str,
    member_name: &str,
    initial_parent_names: &[String],
) -> Result<bool> {
    let mut queue = VecDeque::new();

    if initial_parent_names.is_empty() {
        queue.extend(direct_parent_names_for_class(conn, class_name)?);
        if let Some(engine_conn) = engine_conn {
            queue.extend(direct_parent_names_for_class(engine_conn, class_name)?);
        }
    } else {
        queue.extend(initial_parent_names.iter().cloned());
    }

    let mut visited_names = HashSet::new();

    while let Some(parent_name) = queue.pop_front() {
        let short_name = strip_namespace(&parent_name);
        if short_name.is_empty() || !visited_names.insert(short_name.clone()) {
            continue;
        }

        if member_exists_on_class_name(conn, &short_name, member_name)? {
            return Ok(true);
        }

        if let Some(engine_conn) = engine_conn {
            if member_exists_on_class_name(engine_conn, &short_name, member_name)? {
                return Ok(true);
            }
        }

        queue.extend(direct_parent_names_for_class(conn, &short_name)?);
        if let Some(engine_conn) = engine_conn {
            queue.extend(direct_parent_names_for_class(engine_conn, &short_name)?);
        }
    }

    Ok(false)
}

fn direct_parent_names_for_class(conn: &Connection, class_name: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();

    for class_id in class_ids_by_name(conn, class_name)? {
        for (_, parent_name) in parent_classes(conn, class_id)? {
            if !parent_name.trim().is_empty() {
                names.push(parent_name);
            }
        }
    }

    Ok(names)
}

fn class_ids_by_name(conn: &Connection, class_name: &str) -> Result<Vec<i64>> {
    let short = strip_namespace(class_name);
    if short.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT c.id FROM classes c JOIN strings s ON c.name_id = s.id WHERE s.text = ? ORDER BY c.line_number",
    )?;

    let rows = stmt.query_map([short], |row| row.get::<_, i64>(0))?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn parent_classes(conn: &Connection, class_id: i64) -> Result<Vec<(Option<i64>, String)>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT i.parent_class_id, si.text
        FROM inheritance i
        JOIN strings si ON i.parent_name_id = si.id
        WHERE i.child_id = ?
        "#,
    )?;

    let rows = stmt.query_map([class_id], |row| {
        Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?))
    })?;

    Ok(rows.filter_map(|row| row.ok()).collect())
}

fn member_exists_on_class_name(
    conn: &Connection,
    class_name: &str,
    member_name: &str,
) -> Result<bool> {
    let short = strip_namespace(class_name);
    if short.is_empty() {
        return Ok(false);
    }

    let sql = r#"
        SELECT 1
        FROM members m
        JOIN classes c ON m.class_id = c.id
        JOIN strings sc ON c.name_id = sc.id
        JOIN strings sm ON m.name_id = sm.id
        WHERE sc.text = ?1
          AND sm.text = ?2
        LIMIT 1
    "#;

    Ok(conn
        .query_row(sql, params![short, member_name], |_row| Ok(()))
        .is_ok())
}

fn strip_namespace(name: &str) -> String {
    name.rsplit("::").next().unwrap_or(name).trim().to_string()
}

fn type_references_for_node(node: tree_sitter::Node, content: &str) -> Vec<TypeReference> {
    let mut refs = Vec::new();

    match node.kind() {
        "declaration"
        | "field_declaration"
        | "parameter_declaration"
        | "function_definition"
        | "unreal_function_definition" => {
            if is_forward_declaration_node(node, content) {
                return refs;
            }

            if let Some(type_node) = find_child_by_field(node, "type") {
                refs.extend(type_references_from_type_node(
                    type_node,
                    content,
                    requires_complete_type_for_node(node, content),
                ));
            }
        }
        "base_class_clause" => {
            refs.extend(type_references_from_type_node(node, content, true));
        }
        _ => {}
    }

    refs
}

fn is_forward_declaration_node(node: tree_sitter::Node, content: &str) -> bool {
    if node.kind() != "declaration" {
        return false;
    }

    let text = normalize_space(node_text(node, content));
    if text.is_empty() || !text.ends_with(';') {
        return false;
    }

    Regex::new(
        r"^(?:class|struct|enum(?:\s+class)?)\s+[A-Za-z_][A-Za-z0-9_]*(?:\s*:\s*[A-Za-z_][A-Za-z0-9_:<>]*)?\s*;$",
    )
    .map(|regex| regex.is_match(&text))
    .unwrap_or(false)
}

fn type_references_from_type_node(
    node: tree_sitter::Node,
    content: &str,
    requires_complete: bool,
) -> Vec<TypeReference> {
    let text = node_text(node, content);
    let start = node.start_position();
    let end = node.end_position();

    extract_candidate_type_names(text)
        .into_iter()
        .map(|name| TypeReference {
            name,
            line: start.row as u32,
            character: start.column as u32,
            end_line: end.row as u32,
            end_character: end.column as u32,
            requires_complete,
        })
        .collect()
}

fn requires_complete_type_for_node(node: tree_sitter::Node, content: &str) -> bool {
    match node.kind() {
        "field_declaration" => {
            let text = node_text(node, content);
            !is_indirect_type_usage(text)
        }
        "declaration" => {
            let text = node_text(node, content);
            let has_function_declarator = find_child_by_field(node, "declarator")
                .and_then(|decl| find_child_by_type(decl, "parameter_list"))
                .is_some();
            !has_function_declarator && !is_indirect_type_usage(text)
        }
        _ => false,
    }
}

fn is_indirect_type_usage(text: &str) -> bool {
    let compact = normalize_space(text).replace(' ', "");
    compact.contains('*')
        || compact.contains('&')
        || compact.contains("TObjectPtr<")
        || compact.contains("TWeakObjectPtr<")
        || compact.contains("TSoftObjectPtr<")
        || compact.contains("TSoftClassPtr<")
        || compact.contains("TSubclassOf<")
        || compact.contains("TNonNullSubclassOf<")
        || compact.contains("TScriptInterface<")
}

fn extract_candidate_type_names(text: &str) -> Vec<String> {
    let Some(regex) = Regex::new(r"[A-Za-z_][A-Za-z0-9_:]*").ok() else {
        return Vec::new();
    };

    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for cap in regex.find_iter(text) {
        let token = cap.as_str();
        let short = token.rsplit("::").next().unwrap_or(token).trim();
        if short.is_empty() || is_ignored_type_token(short) {
            continue;
        }
        if seen.insert(short.to_string()) {
            items.push(short.to_string());
        }
    }

    items
}

fn is_ignored_type_token(token: &str) -> bool {
    matches!(
        token,
        "const"
            | "volatile"
            | "class"
            | "struct"
            | "enum"
            | "signed"
            | "unsigned"
            | "short"
            | "long"
            | "void"
            | "bool"
            | "char"
            | "wchar_t"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "int"
            | "float"
            | "double"
            | "auto"
            | "decltype"
            | "typename"
            | "mutable"
            | "static"
            | "virtual"
            | "inline"
            | "FORCEINLINE"
            | "TArray"
            | "TSet"
            | "TMap"
            | "TQueue"
            | "TOptional"
            | "TSharedPtr"
            | "TSharedRef"
            | "TWeakPtr"
            | "TUniquePtr"
            | "TObjectPtr"
            | "TWeakObjectPtr"
            | "TSoftObjectPtr"
            | "TSoftClassPtr"
            | "TSubclassOf"
            | "TNonNullSubclassOf"
            | "TScriptInterface"
            | "TArrayView"
            | "TConstArrayView"
            | "FStringView"
            | "FAnsiStringView"
            | "FWideStringView"
    ) || token.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn is_unreal_implicitly_visible_type(name: &str) -> bool {
    matches!(name, "FObjectInitializer")
}

#[derive(Clone, Debug)]
struct LocalTypeVisibility {
    defined: HashSet<String>,
    forward_declared: HashSet<String>,
}

fn builtin_type_names() -> HashSet<String> {
    [
        "void",
        "bool",
        "char",
        "wchar_t",
        "char8_t",
        "char16_t",
        "char32_t",
        "short",
        "int",
        "long",
        "float",
        "double",
        "size_t",
        "ptrdiff_t",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "UPTRINT",
        "PTRINT",
        "SIZE_T",
        "ANSICHAR",
        "WIDECHAR",
        "UTF8CHAR",
        "TCHAR",
        "FString",
        "FName",
        "FText",
        "FLinearColor",
        "FColor",
        "FVector",
        "FVector2D",
        "FVector4",
        "FRotator",
        "FQuat",
        "FTransform",
    ]
    .into_iter()
    .map(|item| item.to_string())
    .collect()
}

fn collect_local_type_visibility(
    root: tree_sitter::Node,
    content: &str,
 ) -> LocalTypeVisibility {
    let mut defined = builtin_type_names();
    collect_local_type_definitions(root, content, &mut defined);

    let mut forward_declared = HashSet::new();
    collect_forward_declared_types(content, &mut forward_declared);

    LocalTypeVisibility {
        defined,
        forward_declared,
    }
}

fn collect_local_type_definitions(
    node: tree_sitter::Node,
    content: &str,
    visible: &mut HashSet<String>,
) {
    if matches!(
        node.kind(),
        "class_specifier"
            | "struct_specifier"
            | "enum_specifier"
            | "unreal_reflected_class_declaration"
            | "unreal_reflected_struct_declaration"
            | "unreal_reflected_enum_declaration"
    ) {
        let text = node_text(node, content);
        if text.contains('{') {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content).trim().to_string();
                if !name.is_empty() {
                    visible.insert(name);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_local_type_definitions(child, content, visible);
    }
}

fn collect_forward_declared_types(content: &str, visible: &mut HashSet<String>) {
    let Some(regex) = Regex::new(
        r"\b(?:class|struct|enum\s+class|enum)\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s*:\s*[A-Za-z_][A-Za-z0-9_:<>]*)?\s*;"
    ).ok()
    else {
        return;
    };

    for caps in regex.captures_iter(content) {
        if let Some(name) = caps.get(1).map(|m| m.as_str().trim()) {
            if !name.is_empty() {
                visible.insert(name.to_string());
            }
        }
    }
}

fn generated_header_order_diagnostics(content: &str, file_path: &str) -> Vec<DiagnosticItem> {
    let lines: Vec<&str> = content.lines().collect();
    let include_lines = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            parse_include_path(line).map(|path| (index, path))
        })
        .collect::<Vec<_>>();

    let Some((generated_index, generated_path)) = include_lines
        .iter()
        .find(|(_, path)| path.ends_with(".generated.h"))
        .cloned()
    else {
        return Vec::new();
    };

    let Some((later_index, _)) = include_lines
        .iter()
        .find(|(index, _)| *index > generated_index)
        .cloned()
    else {
        return Vec::new();
    };

    vec![
        DiagnosticItem::new(
            Some(file_path),
            generated_index as u32,
            leading_spaces(lines[generated_index]) as u32,
            DiagnosticSeverity::Error,
            "UCore",
            "UHT003",
            format!("{} must be the last include in this header.", generated_path),
        )
        .with_end(generated_index as u32, lines[generated_index].len() as u32),
        DiagnosticItem::new(
            Some(file_path),
            later_index as u32,
            leading_spaces(lines[later_index]) as u32,
            DiagnosticSeverity::Error,
            "UCore",
            "UHT003",
            "No include may appear after a .generated.h include.",
        )
        .with_end(later_index as u32, lines[later_index].len() as u32),
    ]
}

fn source_first_include_diagnostics(
    conn: &Connection,
    content: &str,
    file_path: &str,
) -> Result<Vec<DiagnosticItem>> {
    let lines: Vec<&str> = content.lines().collect();
    let Some((first_include_index, first_include_path)) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| parse_include_path(line).map(|path| (index, path)))
    else {
        return Ok(Vec::new());
    };

    let Some(expected_names) = expected_header_basenames(conn, file_path)? else {
        return Ok(Vec::new());
    };

    let actual_basename = Path::new(&first_include_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if expected_names.contains(&actual_basename) {
        return Ok(Vec::new());
    }

    Ok(vec![
        DiagnosticItem::new(
            Some(file_path),
            first_include_index as u32,
            leading_spaces(lines[first_include_index]) as u32,
            DiagnosticSeverity::Warning,
            "UCore",
            "UECPP006",
            format!(
                "First include in this source file should be its matching header (expected one of: {}).",
                expected_names.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        )
        .with_end(
            first_include_index as u32,
            lines[first_include_index].len() as u32,
        ),
    ])
}

fn expected_header_basenames(conn: &Connection, file_path: &str) -> Result<Option<HashSet<String>>> {
    let normalized = file_path.replace('\\', "/");
    let Some(dot) = normalized.rfind('.') else {
        return Ok(None);
    };
    let base = &normalized[..dot];
    let source_basename = Path::new(base)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if source_basename.is_empty() {
        return Ok(None);
    }

    let mut candidates = HashSet::new();
    for ext in ["h", "hpp", "hh", "hxx"] {
        candidates.insert(format!("{}.{}", source_basename, ext).to_ascii_lowercase());
    }

    let mut matched = false;
    for header_path in source_to_header_candidates(file_path) {
        if Path::new(&header_path).exists() || get_file_id_by_full_path(conn, &header_path).is_some() {
            matched = true;
        }
    }

    if matched {
        Ok(Some(candidates))
    } else {
        Ok(None)
    }
}

fn source_to_header_candidates(path: &str) -> Vec<String> {
    let normalized = path.replace('\\', "/");
    let Some(dot) = normalized.rfind('.') else {
        return Vec::new();
    };
    let base = &normalized[..dot];
    let mut candidates = Vec::new();

    for ext in [".h", ".hpp", ".hh", ".hxx"] {
        candidates.push(format!("{base}{ext}"));
    }

    for mapped in [
        normalized.replace("/Private/", "/Public/"),
        normalized.replace("/Private/", "/Classes/"),
    ] {
        if mapped != normalized {
            let Some(mapped_dot) = mapped.rfind('.') else {
                continue;
            };
            let mapped_base = &mapped[..mapped_dot];
            for ext in [".h", ".hpp", ".hh", ".hxx"] {
                let candidate = format!("{mapped_base}{ext}");
                if !candidates.contains(&candidate) {
                    candidates.insert(0, candidate);
                }
            }
        }
    }

    candidates
}

fn parse_include_path(line: &str) -> Option<String> {
    let regex = Regex::new(r#"#\s*include\s*[<"]([^>"]+)[>"]"#).ok()?;
    regex
        .captures(line)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
}

fn collect_include_paths(content: &str) -> HashSet<String> {
    let Some(regex) = Regex::new(r#"#\s*include\s*[<"]([^>"]+)[>"]"#).ok() else {
        return HashSet::new();
    };

    regex
        .captures_iter(content)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

fn collect_engine_seed_include_paths(
    conn: &Connection,
    project_visible_file_ids: &HashSet<i64>,
    include_paths: &HashSet<String>,
) -> HashSet<String> {
    let mut seeds = include_paths.clone();

    if project_visible_file_ids.is_empty() {
        return seeds;
    }

    let Ok(mut stmt) = conn.prepare_cached(
        r#"
        SELECT s.text
        FROM file_includes fi
        JOIN strings s ON fi.include_path_id = s.id
        WHERE fi.file_id = ?
          AND fi.resolved_file_id IS NULL
        "#,
    ) else {
        return seeds;
    };

    for file_id in project_visible_file_ids {
        if let Ok(rows) = stmt.query_map([file_id], |row| row.get::<_, String>(0)) {
            for include_path in rows.filter_map(|row| row.ok()) {
                let trimmed = include_path.trim();
                if !trimmed.is_empty() {
                    seeds.insert(trimmed.to_string());
                }
            }
        }
    }

    seeds
}

fn lookup_type_visibility(
    conn: &Connection,
    type_name: &str,
    visible_file_ids: Option<&HashSet<i64>>,
    include_paths: Option<&HashSet<String>>,
) -> Result<TypeVisibilityMatch> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            c.file_id,
            CASE
                WHEN dp.full_path IS NULL OR sf.text IS NULL THEN ''
                ELSE dp.full_path || '/' || sf.text
            END
        FROM classes c
        JOIN strings s ON c.name_id = s.id
        LEFT JOIN files f ON c.file_id = f.id
        LEFT JOIN dir_paths dp ON f.directory_id = dp.id
        LEFT JOIN strings sf ON f.filename_id = sf.id
        WHERE s.text = ?1
        "#,
    )?;

    let mut rows = stmt.query(params![type_name])?;
    let mut matched = TypeVisibilityMatch::default();

    while let Some(row) = rows.next()? {
        matched.exists = true;

        let file_id: Option<i64> = row.get(0)?;
        let full_path: String = row.get(1)?;

        if let (Some(visible_file_ids), Some(file_id)) = (visible_file_ids, file_id) {
            if visible_file_ids.contains(&file_id) {
                matched.visible = true;
                return Ok(matched);
            }
        }

        if let Some(include_paths) = include_paths {
            if let Some(include_path) = include_path_from_file_path(&full_path) {
                if include_paths.contains(&include_path) {
                    matched.visible = true;
                    return Ok(matched);
                }
            }
        }
    }

    Ok(matched)
}

fn cached_type_visibility_lookup(
    cache: &mut HashMap<String, TypeVisibilityMatch>,
    conn: &Connection,
    type_name: &str,
    visible_file_ids: Option<&HashSet<i64>>,
    include_paths: Option<&HashSet<String>>,
) -> Result<TypeVisibilityMatch> {
    if let Some(cached) = cache.get(type_name) {
        return Ok(cached.clone());
    }

    let value = lookup_type_visibility(conn, type_name, visible_file_ids, include_paths)?;
    cache.insert(type_name.to_string(), value.clone());
    Ok(value)
}

fn include_path_from_file_path(file_path: &str) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }

    for marker in ["/Public/", "/Classes/", "/Private/"] {
        if let Some(index) = normalized.find(marker) {
            return normalized
                .get(index + marker.len()..)
                .map(|value| value.to_string());
        }
    }

    Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|value| value.to_string())
}

fn reachable_project_file_ids(
    conn: &Connection,
    file_path: &str,
    include_paths: &HashSet<String>,
) -> HashSet<i64> {
    let mut roots = Vec::new();

    if let Some(root_id) = get_file_id_by_full_path(conn, file_path) {
        roots.push(root_id);
    }

    for include_path in include_paths {
        roots.extend(file_ids_by_include_path(conn, include_path));
    }

    reachable_file_ids_from_roots(conn, roots)
}

fn reachable_engine_file_ids(conn: &Connection, include_paths: &HashSet<String>) -> HashSet<i64> {
    let mut roots = Vec::new();
    for include_path in include_paths {
        roots.extend(file_ids_by_include_path(conn, include_path));
    }

    reachable_file_ids_from_roots(conn, roots)
}

fn visible_type_declared_in_files(
    conn: &Connection,
    type_name: &str,
    visible_file_ids: &HashSet<i64>,
    cache: &mut HashMap<String, bool>,
    scan_limit: usize,
) -> Result<bool> {
    if let Some(cached) = cache.get(type_name) {
        return Ok(*cached);
    }

    let declared = type_declared_in_file_set(conn, type_name, visible_file_ids, scan_limit)?;
    cache.insert(type_name.to_string(), declared);
    Ok(declared)
}

fn visible_type_forward_declared_in_files(
    conn: &Connection,
    type_name: &str,
    visible_file_ids: &HashSet<i64>,
    cache: &mut HashMap<String, bool>,
    scan_limit: usize,
) -> Result<bool> {
    if let Some(cached) = cache.get(type_name) {
        return Ok(*cached);
    }

    let declared = type_forward_declared_in_file_set(conn, type_name, visible_file_ids, scan_limit)?;
    cache.insert(type_name.to_string(), declared);
    Ok(declared)
}

fn type_declared_in_file_set(
    conn: &Connection,
    type_name: &str,
    visible_file_ids: &HashSet<i64>,
    scan_limit: usize,
) -> Result<bool> {
    if visible_file_ids.is_empty() || type_name.trim().is_empty() || scan_limit == 0 {
        return Ok(false);
    }

    let declaration_pattern = Regex::new(&format!(
        r"\b(?:class|struct|enum(?:\s+class)?)\s+(?:[A-Za-z_][A-Za-z0-9_]*\s+)*{}\b",
        regex::escape(type_name)
    ))?;

    let mut stmt = conn.prepare(
        r#"
        SELECT
            CASE
                WHEN dp.full_path IS NULL OR sf.text IS NULL THEN ''
                ELSE dp.full_path || '/' || sf.text
            END
        FROM files f
        LEFT JOIN dir_paths dp ON f.directory_id = dp.id
        LEFT JOIN strings sf ON f.filename_id = sf.id
        WHERE f.id = ?1
        "#,
    )?;

    for file_id in visible_file_ids.iter().take(scan_limit) {
        let full_path: String = match stmt.query_row([file_id], |row| row.get(0)) {
            Ok(path) => path,
            Err(_) => continue,
        };

        if full_path.is_empty() {
            continue;
        }

        let Ok(text) = fs::read_to_string(&full_path) else {
            continue;
        };

        if declaration_pattern.is_match(&text) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn type_forward_declared_in_file_set(
    conn: &Connection,
    type_name: &str,
    visible_file_ids: &HashSet<i64>,
    scan_limit: usize,
) -> Result<bool> {
    if visible_file_ids.is_empty() || type_name.trim().is_empty() || scan_limit == 0 {
        return Ok(false);
    }

    let forward_decl_pattern = Regex::new(&format!(
        r"\b(?:class|struct|enum(?:\s+class)?)\s+{}\b(?:\s*:\s*[A-Za-z_][A-Za-z0-9_:<>]*)?\s*;",
        regex::escape(type_name)
    ))?;

    let mut stmt = conn.prepare(
        r#"
        SELECT
            CASE
                WHEN dp.full_path IS NULL OR sf.text IS NULL THEN ''
                ELSE dp.full_path || '/' || sf.text
            END
        FROM files f
        LEFT JOIN dir_paths dp ON f.directory_id = dp.id
        LEFT JOIN strings sf ON f.filename_id = sf.id
        WHERE f.id = ?1
        "#,
    )?;

    for file_id in visible_file_ids.iter().take(scan_limit) {
        let full_path: String = match stmt.query_row([file_id], |row| row.get(0)) {
            Ok(path) => path,
            Err(_) => continue,
        };

        if full_path.is_empty() {
            continue;
        }

        let Ok(text) = fs::read_to_string(&full_path) else {
            continue;
        };

        if forward_decl_pattern.is_match(&text) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn reachable_file_ids_from_roots(conn: &Connection, roots: Vec<i64>) -> HashSet<i64> {
    if roots.is_empty() {
        return HashSet::new();
    }

    let mut included = HashSet::new();
    let mut queue = VecDeque::from(roots);

    while let Some(file_id) = queue.pop_front() {
        if !included.insert(file_id) {
            continue;
        }

        if let Ok(mut stmt) = conn.prepare_cached(
            "SELECT resolved_file_id FROM file_includes WHERE file_id = ? AND resolved_file_id IS NOT NULL",
        ) {
            if let Ok(rows) = stmt.query_map([file_id], |row| row.get::<_, i64>(0)) {
                for id in rows.filter_map(|row| row.ok()) {
                    if !included.contains(&id) {
                        queue.push_back(id);
                    }
                }
            }
        }
    }

    included
}

fn get_file_id_by_full_path(conn: &Connection, file_path: &str) -> Option<i64> {
    let normalized = file_path.replace('\\', "/");
    let sql = r#"
        SELECT f.id
        FROM files f
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sf ON f.filename_id = sf.id
        WHERE
            CASE
                WHEN dp.full_path = '/' THEN '/' || sf.text
                WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                ELSE dp.full_path || '/' || sf.text
            END = ?
        LIMIT 1
    "#;

    conn.query_row(sql, [&normalized], |row| row.get::<_, i64>(0))
        .ok()
}

fn file_ids_by_include_path(conn: &Connection, include_path: &str) -> Vec<i64> {
    let normalized = include_path.replace('\\', "/");
    let basename = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    if basename.is_empty() {
        return Vec::new();
    }

    let sql = r#"
        SELECT DISTINCT f.id
        FROM files f
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sf ON f.filename_id = sf.id
        WHERE sf.text = ?1
           OR (
                CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END LIKE ?2
              )
    "#;

    let like_pattern = format!("%/{}", normalized);
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    match stmt.query_map(params![basename, like_pattern], |row| row.get::<_, i64>(0)) {
        Ok(rows) => rows.filter_map(|row| row.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

#[derive(Clone, Debug)]
struct ExpectedDefinition {
    name: String,
    return_type: String,
    message_label: &'static str,
}

#[derive(Clone, Debug, Default)]
struct UnrealFunctionSpec {
    requires_implementation: bool,
    requires_validate: bool,
    blueprint_implementable_only: bool,
}

#[derive(Clone, Debug)]
struct HeaderFunctionDecl {
    file_path: Option<String>,
    line: u32,
    character: u32,
    end_line: u32,
    end_character: u32,
    class_name: String,
    name: String,
    parameters: String,
    return_type: String,
    full_text: String,
    is_const: bool,
    expected_definitions: Vec<ExpectedDefinition>,
}

fn member_function_declaration(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
) -> Option<HeaderFunctionDecl> {
    let text = node_text(node, content).trim().to_string();
    if !matches!(
        node.kind(),
        "field_declaration" | "unreal_function_declaration" | "declaration"
    ) && !text.contains("UFUNCTION")
    {
        return None;
    }

    if has_enclosing_template(node) {
        return None;
    }

    if text.is_empty()
        || text.contains('{')
        || !text.contains(';')
        || is_macro_invocation_statement(&text)
        || contains_token(&text, "inline")
        || contains_token(&text, "FORCEINLINE")
        || contains_token(&text, "friend")
        || text.contains("= 0")
        || text.contains("= delete")
        || text.contains("= default")
    {
        return None;
    }

    let declarator = find_child_by_field(node, "declarator")
        .or_else(|| find_child_by_type(node, "function_declarator"))?;
    let name_node = find_name_node(declarator)?;
    let parameters = find_child_by_type(declarator, "parameter_list")
        .map(|params| node_text(params, content).to_string())
        .unwrap_or_default();

    if parameters.is_empty() {
        return None;
    }

    let class_name = find_enclosing_class_name(node, content)?;
    let name = node_text(name_node, content).trim().to_string();
    if name.is_empty() {
        return None;
    }

    let mut return_type = find_child_by_field(node, "type")
        .map(|type_node| node_text(type_node, content).to_string())
        .unwrap_or_else(|| extract_prefix_type(node, Some(declarator), content));

    if name == class_name || name == format!("~{}", class_name) {
        return_type.clear();
    }

    let start = declaration_start(node, declarator);
    let end = node.end_position();
    let mut decl = HeaderFunctionDecl {
        file_path: Some(file_path.replace('\\', "/")),
        line: start.row as u32,
        character: start.column as u32,
        end_line: end.row as u32,
        end_character: end.column as u32,
        class_name,
        name,
        parameters,
        return_type: return_type.trim().to_string(),
        full_text: text,
        is_const: is_const_member_function(node, content),
        expected_definitions: Vec::new(),
    };
    decl.expected_definitions = expected_definitions(&decl);

    Some(decl)
}

fn is_macro_invocation_statement(text: &str) -> bool {
    let trimmed = text.trim_start();
    let Some(open_paren) = trimmed.find('(') else {
        return false;
    };

    let prefix = trimmed[..open_paren].trim_end();
    if prefix.is_empty() || prefix.contains(' ') || prefix.contains('\t') {
        return false;
    }

    let mut chars = prefix.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_uppercase() {
        return false;
    }

    if !prefix
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        return false;
    }

    let mut depth = 0i32;
    let mut close_index = None;
    for (offset, ch) in trimmed[open_paren..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_index = Some(open_paren + offset + ch.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }

    let Some(close_index) = close_index else {
        return false;
    };

    let trailing = trimmed[close_index..].trim();
    trailing.is_empty() || trailing == ";"
}

fn build_definition_signature(decl: &HeaderFunctionDecl, expected: &ExpectedDefinition) -> String {
    let normalized_parameters = normalize_parameter_signature(&decl.parameters);
    let cleaned_return_type = crate::parser::cpp::clean_type_string(&expected.return_type);
    let mut signature = if cleaned_return_type.is_empty() {
        format!(
            "{}::{}{}",
            decl.class_name, expected.name, normalized_parameters
        )
    } else {
        format!(
            "{} {}::{}{}",
            cleaned_return_type, decl.class_name, expected.name, normalized_parameters
        )
    };

    signature.push_str(&definition_suffix(decl));
    signature
}

fn expected_definitions(decl: &HeaderFunctionDecl) -> Vec<ExpectedDefinition> {
    let spec = parse_ufunction_spec(&decl.full_text);
    if spec.blueprint_implementable_only {
        return Vec::new();
    }

    let base_name = base_unreal_function_name(&decl.name);
    let implementation_name = if spec.requires_implementation {
        format!("{}_Implementation", base_name)
    } else {
        decl.name.clone()
    };

    let mut items = vec![ExpectedDefinition {
        name: implementation_name,
        return_type: decl.return_type.clone(),
        message_label: if spec.requires_implementation {
            "implementation"
        } else {
            "definition"
        },
    }];

    if spec.requires_validate {
        items.push(ExpectedDefinition {
            name: format!("{}_Validate", base_name),
            return_type: "bool".to_string(),
            message_label: "validation function",
        });
    }

    items
}

fn base_unreal_function_name(name: &str) -> &str {
    if let Some(stripped) = name.strip_suffix("_Implementation") {
        return stripped;
    }

    if let Some(stripped) = name.strip_suffix("_Validate") {
        return stripped;
    }

    name
}

fn parse_ufunction_spec(text: &str) -> UnrealFunctionSpec {
    let Some(specifiers) = extract_macro_arguments(text, "UFUNCTION") else {
        return UnrealFunctionSpec::default();
    };

    let has_token = |token: &str| contains_token(&specifiers, token);

    UnrealFunctionSpec {
        requires_implementation: has_token("BlueprintNativeEvent")
            || has_token("Server")
            || has_token("Client")
            || has_token("NetMulticast"),
        requires_validate: has_token("WithValidation"),
        blueprint_implementable_only: has_token("BlueprintImplementableEvent"),
    }
}

fn extract_macro_arguments(text: &str, macro_name: &str) -> Option<String> {
    let start = text.find(macro_name)?;
    let after_name = text.get(start + macro_name.len()..)?;
    let open_offset = after_name.find('(')?;
    let open_index = start + macro_name.len() + open_offset;
    let mut depth = 0i32;

    for (offset, ch) in text[open_index..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let begin = open_index + 1;
                    let end = open_index + offset;
                    return text.get(begin..end).map(|value| value.to_string());
                }
            }
            _ => {}
        }
    }

    None
}

fn declaration_start(
    node: tree_sitter::Node,
    declarator: tree_sitter::Node,
) -> tree_sitter::Point {
    if let Some(type_node) = find_child_by_field(node, "type") {
        return type_node.start_position();
    }

    let node_start = node.start_position();
    let declarator_start = declarator.start_position();

    if declarator_start.row < node_start.row
        || (declarator_start.row == node_start.row && declarator_start.column < node_start.column)
    {
        declarator_start
    } else {
        node_start
    }
}

fn definition_suffix(decl: &HeaderFunctionDecl) -> String {
    let mut suffixes = Vec::new();
    let params_end = decl
        .full_text
        .find(&decl.parameters)
        .map(|start| start + decl.parameters.len());
    let trailing = params_end
        .and_then(|start| decl.full_text.get(start..))
        .unwrap_or("");

    if decl.is_const {
        suffixes.push("const".to_string());
    }

    if let Some(noexcept_text) = extract_noexcept_text(trailing) {
        suffixes.push(noexcept_text);
    }

    if trailing.contains("&&") {
        suffixes.push("&&".to_string());
    } else if trailing.contains('&') {
        suffixes.push("&".to_string());
    }

    if suffixes.is_empty() {
        String::new()
    } else {
        format!(" {}", suffixes.join(" "))
    }
}

fn extract_noexcept_text(trailing: &str) -> Option<String> {
    let noexcept_index = trailing.find("noexcept")?;
    let rest = trailing.get(noexcept_index..)?.trim_start();

    if let Some(paren_start) = rest.find('(') {
        let mut depth = 0i32;
        for (index, ch) in rest.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && index >= paren_start {
                        return Some(rest[..=index].trim().to_string());
                    }
                }
                _ => {}
            }
        }
    }

    Some("noexcept".to_string())
}

fn has_definition_text(source_text: &str, signature: &str) -> bool {
    let normalized_signature = normalize_space(signature);
    !normalized_signature.is_empty() && source_text.contains(&normalized_signature)
}

fn has_indexed_definition(
    conn: &Connection,
    decl: &HeaderFunctionDecl,
    expected: &ExpectedDefinition,
) -> Result<bool> {
    let expected_params = normalize_parameter_signature(&decl.parameters);
    let expected_return =
        normalize_space(&crate::parser::cpp::clean_type_string(&expected.return_type));
    let mut stmt = conn.prepare(
        r#"
        SELECT
            COALESCE(m.access, ''),
            COALESCE(f.extension, ''),
            COALESCE(m.detail, ''),
            COALESCE(srt.text, '')
        FROM members m
        JOIN classes c ON m.class_id = c.id
        JOIN strings sc ON c.name_id = sc.id
        JOIN strings sm ON m.name_id = sm.id
        JOIN strings st ON m.type_id = st.id
        LEFT JOIN strings srt ON m.return_type_id = srt.id
        LEFT JOIN files f ON COALESCE(m.file_id, c.file_id) = f.id
        WHERE sc.text = ?1
          AND sm.text = ?2
          AND st.text = 'function'
        "#,
    )?;
    let mut rows = stmt.query(params![decl.class_name, expected.name])?;

    while let Some(row) = rows.next()? {
        let access: String = row.get(0)?;
        let extension: String = row.get(1)?;
        let detail: String = row.get(2)?;
        let return_type: String = row.get(3)?;

        if !is_indexed_impl_candidate(&access, &extension) {
            continue;
        }

        if detail.is_empty() || normalize_parameter_signature(&detail) != expected_params {
            continue;
        }

        if !expected_return.is_empty() {
            let actual_return = normalize_space(&crate::parser::cpp::clean_type_string(&return_type));
            if !actual_return.is_empty() && actual_return != expected_return {
                continue;
            }
        }

        return Ok(true);
    }

    Ok(false)
}

fn is_indexed_impl_candidate(access: &str, extension: &str) -> bool {
    matches!(access, "impl")
        || matches!(
            extension.to_ascii_lowercase().as_str(),
            "cpp" | "cc" | "cxx"
        )
}

fn normalize_space(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_parameter_signature(params: &str) -> String {
    let mut out = String::with_capacity(params.len());
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut skipping_default = false;

    for ch in params.chars() {
        if skipping_default {
            match ch {
                '(' => paren_depth += 1,
                ')' => {
                    if paren_depth == 1 && angle_depth == 0 && brace_depth == 0 && bracket_depth == 0 {
                        skipping_default = false;
                        paren_depth -= 1;
                        out.push(')');
                    } else {
                        paren_depth -= 1;
                    }
                }
                '<' => angle_depth += 1,
                '>' => angle_depth = (angle_depth - 1).max(0),
                '{' => brace_depth += 1,
                '}' => brace_depth = (brace_depth - 1).max(0),
                '[' => bracket_depth += 1,
                ']' => bracket_depth = (bracket_depth - 1).max(0),
                ',' if paren_depth == 1 && angle_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                    skipping_default = false;
                    out.push(',');
                }
                _ => {}
            }
            continue;
        }

        match ch {
            '(' => {
                paren_depth += 1;
                out.push(ch);
            }
            ')' => {
                paren_depth -= 1;
                out.push(ch);
            }
            '<' => {
                angle_depth += 1;
                out.push(ch);
            }
            '>' => {
                angle_depth = (angle_depth - 1).max(0);
                out.push(ch);
            }
            '{' => {
                brace_depth += 1;
                out.push(ch);
            }
            '}' => {
                brace_depth = (brace_depth - 1).max(0);
                out.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                out.push(ch);
            }
            ']' => {
                bracket_depth = (bracket_depth - 1).max(0);
                out.push(ch);
            }
            '=' if paren_depth >= 1 && angle_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                skipping_default = true;
            }
            _ => out.push(ch),
        }
    }

    normalize_space(&out)
        .replace(" )", ")")
        .replace(" ,", ",")
}


fn is_header_file(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("h" | "hpp" | "hh" | "hxx")
    )
}

fn is_source_file(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("cpp" | "cc" | "cxx")
    )
}

fn is_cpp_source_or_header(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("h" | "hpp" | "hh" | "hxx" | "cpp" | "cc" | "cxx")
    )
}

fn header_to_source_candidates(path: &str) -> Vec<String> {
    let normalized = path.replace('\\', "/");
    let Some(dot) = normalized.rfind('.') else {
        return Vec::new();
    };

    let base = &normalized[..dot];
    let mut candidates = Vec::new();

    for ext in [".cpp", ".cc", ".cxx"] {
        candidates.push(format!("{base}{ext}"));
    }

    let mapped = normalized
        .replace("/Classes/", "/Private/")
        .replace("/Public/", "/Private/");
    if mapped != normalized {
        let Some(mapped_dot) = mapped.rfind('.') else {
            return candidates;
        };
        let mapped_base = &mapped[..mapped_dot];
        for ext in [".cpp", ".cc", ".cxx"] {
            let candidate = format!("{mapped_base}{ext}");
            if !candidates.contains(&candidate) {
                candidates.insert(0, candidate);
            }
        }
    }

    candidates
}

fn expected_source_path(class_name: &str) -> String {
    format!("{class_name}.cpp")
}

fn has_enclosing_template(node: tree_sitter::Node) -> bool {
    let mut current = node.parent();

    while let Some(parent) = current {
        if parent.kind() == "template_declaration" {
            return true;
        }
        current = parent.parent();
    }

    false
}

fn find_child_by_field<'a>(node: tree_sitter::Node<'a>, field: &str) -> Option<tree_sitter::Node<'a>> {
    node.child_by_field_name(field)
}

fn find_child_by_type<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }

        if let Some(found) = find_child_by_type(child, kind) {
            return Some(found);
        }
    }

    None
}

fn find_name_node(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node),
        "qualified_identifier" => node.child_by_field_name("name"),
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator" => node
            .child_by_field_name("declarator")
            .and_then(find_name_node),
        _ => {
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                if let Some(found) = find_name_node(child) {
                    return Some(found);
                }
            }

            None
        }
    }
}

fn find_enclosing_class_name(node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut current = node.parent();

    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_specifier"
                | "struct_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
        ) {
            if let Some(name_node) = parent.child_by_field_name("name") {
                return Some(node_text(name_node, content).trim().to_string());
            }
        }

        current = parent.parent();
    }

    None
}

fn enclosing_class_base_names(node: tree_sitter::Node, content: &str) -> Vec<String> {
    let mut current = node.parent();

    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_specifier"
                | "struct_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
        ) {
            return extract_base_names_from_class_text(node_text(parent, content));
        }

        current = parent.parent();
    }

    Vec::new()
}

fn extract_base_names_from_class_text(text: &str) -> Vec<String> {
    let head = text.split('{').next().unwrap_or(text);
    let Some((_, bases)) = head.split_once(':') else {
        return Vec::new();
    };

    bases
        .split(',')
        .filter_map(|segment| {
            segment
                .split_whitespace()
                .rev()
                .find(|token| {
                    !matches!(
                        *token,
                        "public" | "protected" | "private" | "virtual" | "final"
                    )
                })
                .map(|token| token.trim_matches(|ch: char| ch == ',' || ch == ':'))
                .filter(|token| !token.is_empty())
                .map(|token| token.to_string())
        })
        .collect()
}

fn extract_prefix_type(
    node: tree_sitter::Node,
    declarator: Option<tree_sitter::Node>,
    content: &str,
) -> String {
    let Some(declarator) = declarator else {
        return String::new();
    };

    let start = node.start_byte();
    let end = declarator.start_byte();

    if end <= start || end > content.len() {
        return String::new();
    }

    content[start..end]
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn is_const_member_function(node: tree_sitter::Node, content: &str) -> bool {
    let Some(declarator) = find_child_by_field(node, "declarator") else {
        return false;
    };

    let Some(params) = find_child_by_type(declarator, "parameter_list") else {
        return false;
    };

    let after_params_start = params.end_byte();
    let node_end = node.end_byte();

    if after_params_start >= node_end || node_end > content.len() {
        return false;
    }

    contains_token(&content[after_params_start..node_end], "const")
}

fn contains_token(text: &str, token: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|part| part == token)
}

fn node_text<'a>(node: tree_sitter::Node, content: &'a str) -> &'a str {
    let range = node.byte_range();

    if range.end <= content.len()
        && content.is_char_boundary(range.start)
        && content.is_char_boundary(range.end)
    {
        &content[range.start..range.end]
    } else {
        ""
    }
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
        || text.starts_with("UINTERFACE(")
        || text.starts_with("USTRUCT(")
        || text.starts_with("UENUM(")
        || text == "UCLASS"
        || text == "UINTERFACE"
        || text == "USTRUCT"
        || text == "UENUM"
}

fn macro_matches_declaration(macro_line: &str, declaration: &str) -> bool {
    if macro_line.starts_with("UCLASS") || macro_line.starts_with("UINTERFACE") {
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
        .any(|line| {
            line.contains("GENERATED_BODY")
                || line.contains("GENERATED_UCLASS_BODY")
                || line.contains("GENERATED_UINTERFACE_BODY")
        })
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

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn insert_string(conn: &Connection, text: &str) -> i64 {
        conn.execute("INSERT OR IGNORE INTO strings (text) VALUES (?)", [text])
            .unwrap();
        conn.query_row("SELECT id FROM strings WHERE text = ?", [text], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn insert_class(conn: &Connection, name: &str) -> i64 {
        let name_id = insert_string(conn, name);
        conn.execute(
            "INSERT INTO classes (name_id, symbol_type, line_number, end_line_number) VALUES (?, 'class', 1, 1)",
            [name_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_class_in_file(conn: &Connection, name: &str, file_id: i64) -> i64 {
        let name_id = insert_string(conn, name);
        conn.execute(
            "INSERT INTO classes (name_id, file_id, symbol_type, line_number, end_line_number) VALUES (?, ?, 'class', 1, 1)",
            rusqlite::params![name_id, file_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_inheritance(
        conn: &Connection,
        child_id: i64,
        parent_name: &str,
        parent_id: Option<i64>,
    ) {
        let parent_name_id = insert_string(conn, parent_name);
        conn.execute(
            "INSERT INTO inheritance (child_id, parent_name_id, parent_class_id) VALUES (?, ?, ?)",
            rusqlite::params![child_id, parent_name_id, parent_id],
        )
        .unwrap();
    }

    fn insert_impl_member(
        conn: &Connection,
        class_id: i64,
        name: &str,
        params_text: &str,
        return_type: Option<&str>,
    ) {
        let name_id = insert_string(conn, name);
        let type_id = insert_string(conn, "function");
        let return_type_id = return_type.map(|text| insert_string(conn, text));
        conn.execute(
            "INSERT INTO members
             (class_id, name_id, type_id, access, detail, return_type_id, line_number)
             VALUES (?, ?, ?, 'impl', ?, ?, 1)",
            rusqlite::params![class_id, name_id, type_id, params_text, return_type_id],
        )
        .unwrap();
    }

    fn insert_decl_member(
        conn: &Connection,
        class_id: i64,
        name: &str,
        params_text: &str,
        return_type: Option<&str>,
    ) {
        let name_id = insert_string(conn, name);
        let type_id = insert_string(conn, "function");
        let return_type_id = return_type.map(|text| insert_string(conn, text));
        conn.execute(
            "INSERT INTO members
             (class_id, name_id, type_id, access, detail, return_type_id, line_number)
             VALUES (?, ?, ?, 'public', ?, ?, 1)",
            rusqlite::params![class_id, name_id, type_id, params_text, return_type_id],
        )
        .unwrap();
    }

    fn insert_header_file(conn: &Connection, subdir: &str, filename: &str) -> i64 {
        let drive = insert_string(conn, "C:");
        let engine_name = insert_string(conn, "Engine");
        let source_name = insert_string(conn, "Source");
        let runtime_name = insert_string(conn, "Runtime");
        let subdir_name = insert_string(conn, subdir);
        let filename_id = insert_string(conn, filename);

        let c_dir = get_or_create_dir(conn, None, drive);
        let engine_dir = get_or_create_dir(conn, Some(c_dir), engine_name);
        let source_dir = get_or_create_dir(conn, Some(engine_dir), source_name);
        let runtime_dir = get_or_create_dir(conn, Some(source_dir), runtime_name);
        let subdir_dir = get_or_create_dir(conn, Some(runtime_dir), subdir_name);

        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?, ?, 'h', 1)",
            rusqlite::params![subdir_dir, filename_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_project_header_file(conn: &Connection, module: &str, filename: &str) -> i64 {
        let drive = insert_string(conn, "C:");
        let project_name = insert_string(conn, "Project");
        let source_name = insert_string(conn, "Source");
        let module_name = insert_string(conn, module);
        let public_name = insert_string(conn, "Public");
        let filename_id = insert_string(conn, filename);

        let c_dir = get_or_create_dir(conn, None, drive);
        let project_dir = get_or_create_dir(conn, Some(c_dir), project_name);
        let source_dir = get_or_create_dir(conn, Some(project_dir), source_name);
        let module_dir = get_or_create_dir(conn, Some(source_dir), module_name);
        let public_dir = get_or_create_dir(conn, Some(module_dir), public_name);

        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?, ?, 'h', 1)",
            rusqlite::params![public_dir, filename_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_file_at_path(conn: &Connection, path: &Path, is_header: bool) -> i64 {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let parts = normalized.split('/').filter(|part| !part.is_empty()).collect::<Vec<_>>();
        assert!(!parts.is_empty(), "path must contain a filename");

        let filename = parts.last().copied().unwrap();
        let filename_id = insert_string(conn, filename);
        let extension = Path::new(filename)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_string();

        let mut parent_id = None;
        for part in &parts[..parts.len() - 1] {
            let name_id = insert_string(conn, part);
            parent_id = Some(get_or_create_dir(conn, parent_id, name_id));
        }

        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?, ?, ?, ?)",
            rusqlite::params![parent_id, filename_id, extension, if is_header { 1 } else { 0 }],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn get_or_create_dir(conn: &Connection, parent_id: Option<i64>, name_id: i64) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO directories (parent_id, name_id) VALUES (?, ?)",
            rusqlite::params![parent_id, name_id],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM directories WHERE parent_id IS ?1 AND name_id = ?2",
            rusqlite::params![parent_id, name_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn insert_include_decl(
        conn: &Connection,
        file_id: i64,
        include_path: &str,
        resolved_file_id: Option<i64>,
    ) {
        let include_path_id = insert_string(conn, include_path);
        let base_filename = Path::new(include_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(include_path);
        let base_filename_id = insert_string(conn, base_filename);
        conn.execute(
            "INSERT INTO file_includes (file_id, include_path_id, base_filename_id, resolved_file_id) VALUES (?, ?, ?, ?)",
            rusqlite::params![file_id, include_path_id, base_filename_id, resolved_file_id],
        )
        .unwrap();
    }

    fn insert_include_edge(conn: &Connection, file_id: i64, include_path: &str, resolved_file_id: i64) {
        insert_include_decl(conn, file_id, include_path, Some(resolved_file_id));
    }

    fn temp_project_path(name: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ucore_diag_{name}_{stamp}"))
    }

    #[test]
    fn detects_missing_generated_body() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            None,
            "UCLASS()\nclass AThing : public UObject {\n};\n",
            Some("C:/Project/AThing.h".to_string()),
            &[],
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UHT002"));
    }

    #[test]
    fn detects_missing_return_in_non_void_function() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            None,
            "int32 ComputeValue()\n{\n    const int32 Local = 42;\n}\n",
            Some("C:/Project/Source/Game/MyActor.cpp".to_string()),
            &[],
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP003"));
    }

    #[test]
    fn does_not_warn_when_non_void_function_returns_on_all_if_branches() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            None,
            "int32 ComputeValue(bool bFlag)\n{\n    if (bFlag)\n    {\n        return 1;\n    }\n    else\n    {\n        return 2;\n    }\n}\n",
            Some("C:/Project/Source/Game/MyActor.cpp".to_string()),
            &[],
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP003"));
    }

    #[test]
    fn does_not_warn_for_void_function_without_return() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            None,
            "void RunTask()\n{\n    const int32 Local = 42;\n}\n",
            Some("C:/Project/Source/Game/MyActor.cpp".to_string()),
            &[],
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP003"));
    }

    #[test]
    fn warns_when_known_type_is_not_visible_in_header() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyDependency");
        insert_impl_member(&conn, class_id, "DoThing", "()", Some("void"));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    UMyDependency Value;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_when_engine_type_is_visible_via_transitive_include() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let texture_file_id = insert_header_file(&engine_conn, "Engine", "Texture2D.h");
        let owner_file_id = insert_header_file(&engine_conn, "Engine", "TextureOwner.h");
        insert_include_edge(&engine_conn, owner_file_id, "Engine/Texture2D.h", texture_file_id);

        let texture_class_id = insert_class(&engine_conn, "UTexture2D");
        engine_conn
            .execute(
                "UPDATE classes SET file_id = ? WHERE id = ?",
                rusqlite::params![texture_file_id, texture_class_id],
            )
            .unwrap();

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "#include \"Engine/TextureOwner.h\"\n\nclass UMyActor\n{\npublic:\n    UTexture2D* Texture;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_when_engine_type_is_visible_via_project_transitive_include() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let current_file_id = insert_project_header_file(&project_conn, "Game", "MyActor.h");
        let shared_file_id = insert_project_header_file(&project_conn, "Game", "SharedTypes.h");
        insert_include_edge(&project_conn, current_file_id, "SharedTypes.h", shared_file_id);
        insert_include_decl(&project_conn, shared_file_id, "CoreMinimal.h", None);

        let core_minimal_file_id = insert_header_file(&engine_conn, "Core", "CoreMinimal.h");
        let object_file_id = insert_header_file(&engine_conn, "CoreUObject", "Object.h");
        insert_include_edge(&engine_conn, core_minimal_file_id, "UObject/Object.h", object_file_id);

        let _object_class_id = insert_class_in_file(&engine_conn, "UObject", object_file_id);

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "#include \"SharedTypes.h\"\n\nclass UMyActor\n{\npublic:\n    UObject* Object;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_for_fobjectinitializer_visibility() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    explicit UMyActor(const FObjectInitializer& ObjectInitializer = FObjectInitializer::Get());\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| {
            item["code"] == "UECPP004"
                && item["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("FObjectInitializer")
        }));
    }

    #[test]
    fn does_not_warn_when_unsynced_current_file_sees_engine_type_via_project_include() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let shared_file_id = insert_project_header_file(&project_conn, "Game", "SharedTypes.h");
        insert_include_decl(&project_conn, shared_file_id, "CoreMinimal.h", None);

        let core_minimal_file_id = insert_header_file(&engine_conn, "Core", "CoreMinimal.h");
        let object_file_id = insert_header_file(&engine_conn, "CoreUObject", "Object.h");
        insert_include_edge(&engine_conn, core_minimal_file_id, "UObject/Object.h", object_file_id);
        let _object_class_id = insert_class_in_file(&engine_conn, "UObject", object_file_id);

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "#include \"SharedTypes.h\"\n\nclass UMyActor\n{\npublic:\n    UObject* Object;\n};\n",
            Some("C:/Project/Source/Game/Public/NewUnsyncedActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_when_visible_engine_header_text_declares_type_but_class_record_points_to_cpp() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let root = temp_project_path("visible_text_declares_type");
        let blueprint_dir = root.join("Engine/Source/Runtime/UMG/Public/Blueprint");
        let components_dir = root.join("Engine/Source/Runtime/UMG/Public/Components");
        let private_dir = root.join("Engine/Source/Runtime/UMG/Private/Components");
        std::fs::create_dir_all(&blueprint_dir).unwrap();
        std::fs::create_dir_all(&components_dir).unwrap();
        std::fs::create_dir_all(&private_dir).unwrap();

        let user_widget_path = blueprint_dir.join("UserWidget.h");
        let widget_header_path = components_dir.join("Widget.h");
        let widget_cpp_path = private_dir.join("Widget.cpp");

        std::fs::write(
            &user_widget_path,
            "#pragma once\n#include \"Components/Widget.h\"\nclass UUserWidget : public UWidget {};\n",
        )
        .unwrap();
        std::fs::write(&widget_header_path, "#pragma once\nclass UWidget {};\n").unwrap();
        std::fs::write(&widget_cpp_path, "// impl\n").unwrap();

        let user_widget_file_id = insert_file_at_path(&engine_conn, &user_widget_path, true);
        let widget_header_file_id = insert_file_at_path(&engine_conn, &widget_header_path, true);
        let widget_cpp_file_id = insert_file_at_path(&engine_conn, &widget_cpp_path, false);
        insert_include_edge(
            &engine_conn,
            user_widget_file_id,
            "Components/Widget.h",
            widget_header_file_id,
        );

        let widget_class_id = insert_class_in_file(&engine_conn, "UWidget", widget_cpp_file_id);
        engine_conn
            .execute(
                "UPDATE classes SET file_id = ? WHERE id = ?",
                rusqlite::params![widget_cpp_file_id, widget_class_id],
            )
            .unwrap();

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "#include \"Blueprint/UserWidget.h\"\n\nclass UMyActor\n{\npublic:\n    UWidget* Widget;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn warns_when_forward_declared_type_is_used_by_value() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyDependency");
        insert_impl_member(&conn, class_id, "DoThing", "()", Some("void"));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyDependency;\n\nclass UMyActor\n{\npublic:\n    UMyDependency Value;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP005"));
    }

    #[test]
    fn does_not_warn_on_forward_declaration_itself() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyDependency;\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
        assert!(!items.iter().any(|item| item["code"] == "UECPP005"));
    }

    #[test]
    fn does_not_warn_when_type_is_forward_declared() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyDependency");
        insert_impl_member(&conn, class_id, "DoThing", "()", Some("void"));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyDependency;\n\nclass UMyActor\n{\npublic:\n    UMyDependency* Value;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_when_type_is_forward_declared_via_included_header() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyDependency");
        insert_impl_member(&conn, class_id, "DoThing", "()", Some("void"));

        let root = temp_project_path("forward_declared_via_include");
        let public_dir = root.join("Source/Game/Public");
        std::fs::create_dir_all(&public_dir).unwrap();
        let shared_header = public_dir.join("SharedTypes.h");
        let actor_header = public_dir.join("MyActor.h");
        std::fs::write(&shared_header, "class UMyDependency;\n").unwrap();
        std::fs::write(
            &actor_header,
            "#include \"SharedTypes.h\"\n\nclass UMyActor\n{\npublic:\n    UMyDependency* Value;\n};\n",
        )
        .unwrap();

        let current_file_id = insert_file_at_path(&conn, &actor_header, true);
        let shared_file_id = insert_file_at_path(&conn, &shared_header, true);
        insert_include_edge(&conn, current_file_id, "SharedTypes.h", shared_file_id);

        let value = process_diagnostics(
            &conn,
            None,
            "#include \"SharedTypes.h\"\n\nclass UMyActor\n{\npublic:\n    UMyDependency* Value;\n};\n",
            Some(actor_header.to_string_lossy().to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_when_enum_class_with_underlying_type_is_forward_declared() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "enum class ERShot_Type : uint8;\n\nclass UMyActor\n{\npublic:\n    ERShot_Type Value;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn does_not_warn_when_forward_declared_type_is_wrapped_in_tobjectptr() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyDependency");
        insert_impl_member(&conn, class_id, "DoThing", "()", Some("void"));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyDependency;\n\nclass UMyActor\n{\npublic:\n    TObjectPtr<UMyDependency> Value;\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP005"));
        assert!(!items.iter().any(|item| item["code"] == "UECPP004"));
    }

    #[test]
    fn warns_when_generated_header_is_not_last_include() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let value = process_diagnostics(
            &conn,
            None,
            "#include \"MyActor.generated.h\"\n#include \"SomethingElse.h\"\n\nUCLASS()\nclass UMyActor\n{\n    GENERATED_BODY()\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UHT003"));
    }

    #[test]
    fn warns_when_source_does_not_include_own_header_first() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("source_first_include");
        let public_dir = root.join("Source/Game/Public");
        let private_dir = root.join("Source/Game/Private");
        let header = public_dir.join("MyActor.h");
        std::fs::create_dir_all(&public_dir).unwrap();
        std::fs::create_dir_all(&private_dir).unwrap();
        std::fs::write(&header, "class UMyActor {};\n").unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "#include \"OtherThing.h\"\n#include \"MyActor.h\"\n",
            Some(
                private_dir
                    .join("MyActor.cpp")
                    .to_string_lossy()
                    .replace('\\', "/"),
            ),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP006"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_when_source_includes_own_header_first() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("source_first_include_ok");
        let public_dir = root.join("Source/Game/Public");
        let private_dir = root.join("Source/Game/Private");
        let header = public_dir.join("MyActor.h");
        std::fs::create_dir_all(&public_dir).unwrap();
        std::fs::create_dir_all(&private_dir).unwrap();
        std::fs::write(&header, "class UMyActor {};\n").unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "#include \"MyActor.h\"\n#include \"OtherThing.h\"\n",
            Some(
                private_dir
                    .join("MyActor.cpp")
                    .to_string_lossy()
                    .replace('\\', "/"),
            ),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP006"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_for_valid_override_name() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let base_id = insert_class(&conn, "UBaseAbility");
        insert_decl_member(&conn, base_id, "EndAbility", "()", Some("void"));

        let child_id = insert_class(&conn, "UMyAbility");
        insert_inheritance(&conn, child_id, "UBaseAbility", Some(base_id));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyAbility : public UBaseAbility\n{\npublic:\n    virtual void EndAbility() override;\n};\n",
            Some("C:/Project/Source/Game/Public/MyAbility.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP007"));
    }

    #[test]
    fn warns_when_override_has_no_base_member_name() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let base_id = insert_class(&conn, "UBaseAbility");
        insert_decl_member(&conn, base_id, "EndAbility", "()", Some("void"));

        let child_id = insert_class(&conn, "UMyAbility");
        insert_inheritance(&conn, child_id, "UBaseAbility", Some(base_id));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyAbility : public UBaseAbility\n{\npublic:\n    virtual void StopAbility() override;\n};\n",
            Some("C:/Project/Source/Game/Public/MyAbility.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP007"));
    }

    #[test]
    fn does_not_warn_for_engine_override_name_on_project_child() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let child_id = insert_class(&project_conn, "UWeaponForgeMain");
        insert_inheritance(&project_conn, child_id, "UBaseWidget", None);

        let base_id = insert_class(&engine_conn, "UBaseWidget");
        insert_decl_member(
            &engine_conn,
            base_id,
            "NativeGetDesiredFocusTarget",
            "() const",
            Some("void"),
        );

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "class UWeaponForgeMain : public UBaseWidget\n{\npublic:\n    virtual void NativeGetDesiredFocusTarget() const override;\n};\n",
            Some("C:/Project/Source/Game/Public/WeaponForgeMain.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP007"));
    }

    #[test]
    fn does_not_warn_for_engine_override_name_when_only_impl_is_indexed() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let child_id = insert_class(&project_conn, "UWeaponForgeMain");
        insert_inheritance(&project_conn, child_id, "UCommonActivatableWidget", None);

        let base_id = insert_class(&engine_conn, "UCommonActivatableWidget");
        insert_impl_member(
            &engine_conn,
            base_id,
            "NativeGetDesiredFocusTarget",
            "() const",
            Some("UWidget"),
        );

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "class UWeaponForgeMain : public UCommonActivatableWidget\n{\npublic:\n    virtual UWidget* NativeGetDesiredFocusTarget() const override;\n};\n",
            Some("C:/Project/Source/Game/Public/WeaponForgeMain.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP007"));
    }

    #[test]
    fn does_not_warn_for_buffer_only_engine_override_name() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let base_id = insert_class(&engine_conn, "UBaseWidget");
        insert_decl_member(
            &engine_conn,
            base_id,
            "NativeGetDesiredFocusTarget",
            "() const",
            Some("void"),
        );

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "class UWeaponForgeMain : public UBaseWidget\n{\npublic:\n    virtual void NativeGetDesiredFocusTarget() const override;\n};\n",
            Some("C:/Project/Source/Game/Public/WeaponForgeMain.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP007"));
    }

    #[test]
    fn does_not_warn_for_override_when_project_parent_leads_to_engine_grandparent() {
        let project_conn = Connection::open_in_memory().unwrap();
        let engine_conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&project_conn).unwrap();
        crate::db::init_db(&engine_conn).unwrap();

        let project_parent_id = insert_class(&project_conn, "UProjectWidgetBase");
        insert_inheritance(&project_conn, project_parent_id, "UBaseWidget", None);

        let engine_base_id = insert_class(&engine_conn, "UBaseWidget");
        insert_decl_member(
            &engine_conn,
            engine_base_id,
            "NativeGetDesiredFocusTarget",
            "() const",
            Some("void"),
        );

        let value = process_diagnostics(
            &project_conn,
            Some(&engine_conn),
            "class UWeaponForgeMain : public UProjectWidgetBase\n{\npublic:\n    virtual void NativeGetDesiredFocusTarget() const override;\n};\n",
            Some("C:/Project/Source/Game/Public/WeaponForgeMain.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP007"));
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
            None,
            "UENUM(BlueprintType)\nenum class EThing { One };\n",
            Some("C:/Project/EThing.h".to_string()),
            &[],
        )
        .unwrap();
        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UHT002"));
    }

    #[test]
    fn detects_missing_cpp_definition_for_header_member_function() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("missing_impl");
        let header = root.join("Source/Game/Public/MyActor.h");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    void DoThing();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incomplete_member_declaration_does_not_warn_about_missing_cpp() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("incomplete_decl");
        let header = root.join("Source/Game/Public/MyActor.h");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    void DoThing()\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));
        assert!(items.iter().any(|item| item["code"] == "UECPP002"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_when_matching_cpp_definition_exists() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("has_impl");
        let header = root.join("Source/Game/Public/MyActor.h");
        let source = root.join("Source/Game/Private/MyActor.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"MyActor.h\"\n\nvoid UMyActor::DoThing()\n{\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    void DoThing();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_when_matching_cpp_definition_exists_in_overlay() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("has_impl_overlay");
        let header = root.join("Source/Game/Public/MyActor.h");
        let source = root.join("Source/Game/Private/MyActor.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "#include \"MyActor.h\"\n").unwrap();

        let overlays = vec![OpenBufferOverlay {
            file_path: source.to_string_lossy().replace('\\', "/"),
            content: "#include \"MyActor.h\"\n\nvoid UMyActor::DoThing()\n{\n}\n".to_string(),
        }];

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    void DoThing();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &overlays,
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn does_not_warn_when_indexed_cpp_definition_exists() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let class_id = insert_class(&conn, "UMyActor");
        insert_impl_member(&conn, class_id, "DoThing", "(int32 Count)", Some("void"));

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    void DoThing(int32 Count);\n};\n",
            Some("C:/Project/Source/Game/Public/MyActor.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));
    }

    #[test]
    fn constructor_with_default_argument_matches_cpp_definition() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("ctor_default_arg");
        let header = root.join("Source/Game/Public/MyActor.h");
        let source = root.join("Source/Game/Private/MyActor.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"MyActor.h\"\n\nUMyActor::UMyActor(const FObjectInitializer& ObjectInitializer)\n{\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyActor\n{\npublic:\n    explicit UMyActor(const FObjectInitializer& ObjectInitializer = FObjectInitializer::Get());\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unreal_style_header_missing_method_warns_without_false_ctor_warning() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("unreal_header_missing_impl");
        let header = root.join("Source/Game/Public/MyAbility.h");
        let source = root.join("Source/Game/Private/MyAbility.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"MyAbility.h\"\n\nUMyAbility::UMyAbility(const FObjectInitializer& ObjectInitializer)\n{\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "UCLASS()\nclass UMyAbility\n{\n    GENERATED_BODY()\npublic:\n    explicit UMyAbility(const FObjectInitializer& ObjectInitializer = FObjectInitializer::Get());\nprivate:\n    void StartDeath();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| {
            item["code"] == "UECPP001"
                && item["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("StartDeath")
        }));
        assert!(!items.iter().any(|item| {
            item["code"] == "UECPP001"
                && item["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("UMyAbility::UMyAbility")
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn blueprint_native_event_uses_implementation_suffix() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("blueprint_native_event_impl");
        let header = root.join("Source/Game/Public/MyAbility.h");
        let source = root.join("Source/Game/Private/MyAbility.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"MyAbility.h\"\n\nvoid UMyAbility::OnDeath_Implementation()\n{\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyAbility\n{\npublic:\n    UFUNCTION(BlueprintNativeEvent)\n    void OnDeath();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn blueprint_implementable_event_does_not_require_cpp_definition() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("blueprint_implementable_event");
        let header = root.join("Source/Game/Public/MyAbility.h");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyAbility\n{\npublic:\n    UFUNCTION(BlueprintImplementableEvent)\n    void OnDeath();\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attribute_accessors_macro_does_not_warn_about_missing_cpp_definition() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class ULyraCombatSet\n{\npublic:\n    ATTRIBUTE_ACCESSORS(ULyraCombatSet, BaseDamage);\n    ATTRIBUTE_ACCESSORS(ULyraCombatSet, BaseHeal);\n};\n",
            Some("C:/Project/Source/Game/Public/LyraCombatSet.h".to_string()),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));
    }

    #[test]
    fn api_and_virtual_prefixes_do_not_break_cpp_definition_match() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("api_virtual_impl");
        let header = root.join("Source/Game/Public/LyraCombatSet.h");
        let source = root.join("Source/Game/Private/LyraCombatSet.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"LyraCombatSet.h\"\n\nbool ULyraCombatSet::PreGameplayEffectExecute(FGameplayEffectModCallbackData& Data)\n{\n    return true;\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class ULyraCombatSet\n{\npublic:\n    UE_API virtual bool PreGameplayEffectExecute(FGameplayEffectModCallbackData& Data) override;\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(!items.iter().any(|item| item["code"] == "UECPP001"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rpc_with_validation_requires_validate_and_implementation() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let root = temp_project_path("rpc_validate_impl");
        let header = root.join("Source/Game/Public/MyAbility.h");
        let source = root.join("Source/Game/Private/MyAbility.cpp");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "#include \"MyAbility.h\"\n\nvoid UMyAbility::ServerFire_Implementation(int32 Count)\n{\n}\n",
        )
        .unwrap();

        let value = process_diagnostics(
            &conn,
            None,
            "class UMyAbility\n{\npublic:\n    UFUNCTION(Server, Reliable, WithValidation)\n    void ServerFire(int32 Count);\n};\n",
            Some(header.to_string_lossy().replace('\\', "/")),
            &[],
        )
        .unwrap();

        let items = value["items"].as_array().unwrap();
        assert!(items.iter().any(|item| {
            item["code"] == "UECPP001"
                && item["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("ServerFire_Validate")
        }));
        assert!(!items.iter().any(|item| {
            item["code"] == "UECPP001"
                && item["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("ServerFire_Implementation")
        }));

        let first = items
            .iter()
            .find(|item| item["code"] == "UECPP001")
            .expect("missing UECPP001");
        assert!(
            first["end_character"].as_u64().unwrap_or(0)
                > first["character"].as_u64().unwrap_or(0)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normalize_parameter_signature_strips_default_arguments() {
        let params = "(const FObjectInitializer& ObjectInitializer = FObjectInitializer::Get())";
        assert_eq!(
            normalize_parameter_signature(params),
            "(const FObjectInitializer& ObjectInitializer)"
        );
    }
}
