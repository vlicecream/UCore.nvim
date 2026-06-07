use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Query};

use crate::diagnostics::{
    DiagnosticItem, DiagnosticRelatedItem, DiagnosticSeverity, LinkerRules,
};
use crate::parser::cpp;
use crate::types::{ClassInfo, MemberInfo};

const SOURCE_NAME: &str = "UCore";
const LNK_ODR_FUNCTION: &str = "UECPP-LNK-001";
const LNK_ODR_CLASS: &str = "UECPP-LNK-002";
const LNK_MISSING_DEF: &str = "UECPP-LNK-003";
const LNK_NAME_CLASH: &str = "UECPP-LNK-004";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedDiagnosticRelatedItem {
    file_path: Option<String>,
    line: u32,
    character: u32,
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedDiagnosticItem {
    file_path: Option<String>,
    line: u32,
    character: u32,
    end_line: u32,
    end_character: u32,
    severity: DiagnosticSeverity,
    code: String,
    message: String,
    #[serde(default)]
    related: Vec<CachedDiagnosticRelatedItem>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LinkerCacheFile {
    db_mtime_secs: u64,
    last_full_check_ms: u128,
    total_issues: usize,
    by_file: BTreeMap<String, Vec<CachedDiagnosticItem>>,
}

#[derive(Clone, Debug)]
struct FunctionSymbol {
    file_path: String,
    line: u32,
    character: u32,
    end_line: u32,
    end_character: u32,
    owner_name: Option<String>,
    name: String,
    signature: String,
    is_definition: bool,
    is_virtual: bool,
    is_template: bool,
    is_inline: bool,
    is_static: bool,
    is_pure_virtual: bool,
    is_ue_reflected: bool,
    generated_header: bool,
}

#[derive(Clone, Debug)]
struct ClassSymbol {
    file_path: String,
    line: u32,
    end_line: u32,
    fq_name: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct NameClashRow {
    file_path: String,
    line: u32,
    kind: String,
}

pub(crate) fn cached_items_for_file(
    project_root: &str,
    primary_db_path: &str,
    file_path: Option<&str>,
    rules: &LinkerRules,
) -> Result<Vec<DiagnosticItem>> {
    if !rules.enabled {
        return Ok(Vec::new());
    }

    let Some(file_path) = file_path.map(normalize_path) else {
        return Ok(Vec::new());
    };

    let cache_path = linker_cache_path(project_root);
    if !cache_path.is_file() {
        return Ok(Vec::new());
    }

    let cache: LinkerCacheFile = serde_json::from_str(
        &fs::read_to_string(&cache_path)
            .with_context(|| format!("failed to read linker cache {}", cache_path.display()))?,
    )
    .with_context(|| format!("failed to parse linker cache {}", cache_path.display()))?;

    if cache.db_mtime_secs != db_mtime_secs(primary_db_path).unwrap_or_default() {
        return Ok(Vec::new());
    }

    Ok(cache
        .by_file
        .get(&file_path)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(cached_item_to_runtime)
        .collect())
}

pub(crate) fn rebuild_full_cache(
    conn: &Connection,
    project_root: &str,
    primary_db_path: &str,
    rules: &LinkerRules,
) -> Result<()> {
    let cache_path = linker_cache_path(project_root);
    if !rules.enabled {
        let _ = fs::remove_file(&cache_path);
        return Ok(());
    }

    let started = Instant::now();
    let files = indexed_project_files(conn)?;
    let symbols = collect_project_symbols(&files)?;
    let mut by_file: BTreeMap<String, Vec<DiagnosticItem>> = BTreeMap::new();

    if rules.check_odr_function {
        for (file, items) in function_odr_items(&symbols.functions, rules) {
            by_file.entry(file).or_default().extend(items);
        }
    }
    if rules.check_odr_class {
        for (file, items) in class_odr_items(&symbols.classes, rules) {
            by_file.entry(file).or_default().extend(items);
        }
    }
    if rules.check_missing_def {
        for (file, items) in missing_definition_items(&symbols.functions, &symbols.class_macro_flags, rules) {
            by_file.entry(file).or_default().extend(items);
        }
    }
    if rules.check_name_clash {
        for (file, items) in name_clash_items(conn, rules)? {
            by_file.entry(file).or_default().extend(items);
        }
    }

    for items in by_file.values_mut() {
        items.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.character.cmp(&right.character))
                .then(left.code.cmp(right.code))
        });
    }

    let cache = LinkerCacheFile {
        db_mtime_secs: db_mtime_secs(primary_db_path).unwrap_or_default(),
        last_full_check_ms: started.elapsed().as_millis(),
        total_issues: by_file.values().map(Vec::len).sum(),
        by_file: by_file
            .into_iter()
            .map(|(path, items)| (path, items.into_iter().map(runtime_item_to_cached).collect()))
            .collect(),
    };

    write_cache(&cache_path, &cache)
}

pub(crate) fn collect_incremental(
    conn: &Connection,
    project_root: &str,
    primary_db_path: &str,
    file_path: &str,
    content: &str,
    rules: &LinkerRules,
) -> Result<Vec<DiagnosticItem>> {
    if !rules.enabled || !rules.incremental_on_save {
        return Ok(Vec::new());
    }
    if content.lines().count() > rules.incremental_on_save_if_larger_than_lines {
        return cached_items_for_file(project_root, primary_db_path, Some(file_path), rules);
    }

    let current_path = normalize_path(file_path);
    let current = collect_symbols_for_content(&current_path, content)?;
    let candidate_paths = candidate_paths_for_incremental(conn, &current_path, &current.functions, &current.classes)?;

    let mut functions = current.functions.clone();
    let mut classes = current.classes.clone();
    let mut class_macro_flags = current.class_macro_flags.clone();

    for path in candidate_paths {
        if path == current_path {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let other = collect_symbols_for_content(&path, &text)?;
        functions.extend(other.functions);
        classes.extend(other.classes);
        class_macro_flags.extend(other.class_macro_flags);
    }

    let mut impacted: BTreeMap<String, Vec<DiagnosticItem>> = BTreeMap::new();
    if rules.check_odr_function {
        for (path, items) in function_odr_items(&functions, rules) {
            impacted.entry(path).or_default().extend(items);
        }
    }
    if rules.check_odr_class {
        for (path, items) in class_odr_items(&classes, rules) {
            impacted.entry(path).or_default().extend(items);
        }
    }
    if rules.check_missing_def {
        for (path, items) in missing_definition_items(&functions, &class_macro_flags, rules) {
            impacted.entry(path).or_default().extend(items);
        }
    }

    let cache_path = linker_cache_path(project_root);
    let mut cache = read_cache_if_fresh(&cache_path, primary_db_path)?.unwrap_or_default();
    let mut affected_paths: BTreeSet<String> = impacted.keys().cloned().collect();
    affected_paths.insert(current_path.clone());

    for path in affected_paths {
        if let Some(items) = impacted.remove(&path) {
            cache.by_file.insert(
                path.clone(),
                items.into_iter().map(runtime_item_to_cached).collect(),
            );
        } else {
            cache.by_file.remove(&path);
        }
    }
    cache.total_issues = cache.by_file.values().map(Vec::len).sum();
    cache.db_mtime_secs = db_mtime_secs(primary_db_path).unwrap_or_default();
    write_cache(&cache_path, &cache)?;

    Ok(cache
        .by_file
        .get(&current_path)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(cached_item_to_runtime)
        .collect())
}

#[derive(Default)]
struct ProjectSymbols {
    functions: Vec<FunctionSymbol>,
    classes: Vec<ClassSymbol>,
    class_macro_flags: HashMap<String, bool>,
}

fn collect_project_symbols(files: &[String]) -> Result<ProjectSymbols> {
    let mut out = ProjectSymbols::default();
    for path in files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let symbols = collect_symbols_for_content(path, &text)?;
        out.functions.extend(symbols.functions);
        out.classes.extend(symbols.classes);
        out.class_macro_flags.extend(symbols.class_macro_flags);
    }
    Ok(out)
}

fn collect_symbols_for_content(file_path: &str, content: &str) -> Result<ProjectSymbols> {
    let functions = collect_function_symbols(file_path, content)?;
    let classes = collect_class_symbols(file_path, content)?;
    let class_macro_flags = classes
        .iter()
        .map(|class| (class.fq_name.clone(), content.contains("GENERATED_BODY(")))
        .collect();
    Ok(ProjectSymbols {
        functions,
        classes,
        class_macro_flags,
    })
}

fn indexed_project_files(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT path
        FROM search_files
        WHERE lower(COALESCE(ext, '')) IN ('h','hh','hpp','hxx','c','cc','cpp','cxx','inl')
        ORDER BY path
        "#,
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows
        .filter_map(|row| row.ok())
        .map(|path| normalize_path(&path))
        .collect())
}

fn candidate_paths_for_incremental(
    conn: &Connection,
    current_file: &str,
    functions: &[FunctionSymbol],
    classes: &[ClassSymbol],
) -> Result<Vec<String>> {
    let mut names = HashSet::new();
    names.extend(functions.iter().map(|func| func.name.clone()));
    names.extend(classes.iter().map(|class| short_name(&class.fq_name).to_string()));
    if names.is_empty() {
        return Ok(Vec::new());
    }

    let mut paths = HashSet::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT DISTINCT path
        FROM search_symbols
        WHERE name = ?1
          AND path != ?2
        "#,
    )?;

    for name in names {
        let rows = stmt.query_map(params![name, current_file], |row| row.get::<_, String>(0))?;
        for row in rows.filter_map(|value| value.ok()) {
            paths.insert(normalize_path(&row));
        }
    }

    let mut out = paths.into_iter().collect::<Vec<_>>();
    out.sort();
    Ok(out)
}

fn function_odr_items(
    functions: &[FunctionSymbol],
    rules: &LinkerRules,
) -> BTreeMap<String, Vec<DiagnosticItem>> {
    let mut groups: HashMap<String, Vec<&FunctionSymbol>> = HashMap::new();
    for function in functions {
        if !function.is_definition
            || function.is_virtual
            || function.is_template
            || function.is_inline
            || function.is_static
            || function.is_ue_reflected
            || function.generated_header
        {
            continue;
        }
        groups.entry(function.signature.clone()).or_default().push(function);
    }

    let severity = DiagnosticSeverity::from(rules.odr_function_severity);
    let mut by_file = BTreeMap::new();
    for defs in groups.into_values() {
        let distinct_files = defs
            .iter()
            .map(|item| item.file_path.as_str())
            .collect::<HashSet<_>>();
        if distinct_files.len() <= 1 {
            continue;
        }

        let related = defs
            .iter()
            .map(|item| DiagnosticRelatedItem {
                file_path: Some(item.file_path.clone()),
                line: item.line,
                character: item.character,
                message: format!("{}:{}", short_file_name(&item.file_path), item.line + 1),
            })
            .collect::<Vec<_>>();
        let locations = defs
            .iter()
            .map(|item| format!("{}:{}", short_file_name(&item.file_path), item.line + 1))
            .collect::<Vec<_>>()
            .join(", ");

        for item in defs {
            by_file
                .entry(item.file_path.clone())
                .or_insert_with(Vec::new)
                .push(
                    DiagnosticItem::new(
                        Some(&item.file_path),
                        item.line,
                        item.character,
                        severity.clone(),
                        SOURCE_NAME,
                        LNK_ODR_FUNCTION,
                        format!(
                            "Function '{}' is defined in multiple files: {}.",
                            display_function_name(item),
                            locations
                        ),
                    )
                    .with_end(item.end_line, item.end_character)
                    .with_related(related.clone()),
                );
        }
    }
    by_file
}

fn class_odr_items(classes: &[ClassSymbol], rules: &LinkerRules) -> BTreeMap<String, Vec<DiagnosticItem>> {
    let mut groups: HashMap<String, Vec<&ClassSymbol>> = HashMap::new();
    for class in classes {
        groups.entry(class.fq_name.clone()).or_default().push(class);
    }

    let severity = DiagnosticSeverity::from(rules.odr_class_severity);
    let mut by_file = BTreeMap::new();
    for (name, defs) in groups {
        let unique = defs
            .iter()
            .map(|item| item.fingerprint.as_str())
            .collect::<HashSet<_>>();
        if unique.len() <= 1 || defs.len() <= 1 {
            continue;
        }

        let related = defs
            .iter()
            .map(|item| DiagnosticRelatedItem {
                file_path: Some(item.file_path.clone()),
                line: item.line,
                character: 0,
                message: format!("{}:{}", short_file_name(&item.file_path), item.line + 1),
            })
            .collect::<Vec<_>>();
        let locations = defs
            .iter()
            .map(|item| format!("{}:{}", short_file_name(&item.file_path), item.line + 1))
            .collect::<Vec<_>>()
            .join(", ");

        for item in defs {
            by_file
                .entry(item.file_path.clone())
                .or_insert_with(Vec::new)
                .push(
                    DiagnosticItem::new(
                        Some(&item.file_path),
                        item.line,
                        0,
                        severity.clone(),
                        SOURCE_NAME,
                        LNK_ODR_CLASS,
                        format!("Type '{}' has divergent definitions across files: {}.", name, locations),
                    )
                    .with_end(item.end_line, 1)
                    .with_related(related.clone()),
                );
        }
    }
    by_file
}

fn missing_definition_items(
    functions: &[FunctionSymbol],
    class_macro_flags: &HashMap<String, bool>,
    rules: &LinkerRules,
) -> BTreeMap<String, Vec<DiagnosticItem>> {
    let mut defs = HashSet::new();
    for function in functions {
        if function.is_definition {
            defs.insert(function.signature.clone());
        }
    }

    let severity = DiagnosticSeverity::from(rules.missing_def_severity);
    let mut by_file = BTreeMap::new();
    for function in functions {
        if function.is_definition
            || function.is_pure_virtual
            || function.is_template
            || function.generated_header
            || function.is_ue_reflected
        {
            continue;
        }
        if !is_header_path(&function.file_path) {
            continue;
        }
        if defs.contains(&function.signature) {
            continue;
        }
        if should_ignore_missing_definition(function, class_macro_flags, rules) {
            continue;
        }

        by_file
            .entry(function.file_path.clone())
            .or_insert_with(Vec::new)
            .push(
                DiagnosticItem::new(
                    Some(&function.file_path),
                    function.line,
                    function.character,
                    severity.clone(),
                    SOURCE_NAME,
                    LNK_MISSING_DEF,
                    format!(
                        "Function '{}' is declared but no matching definition was found.",
                        display_function_name(function)
                    ),
                )
                .with_end(function.end_line, function.end_character),
            );
    }

    by_file
}

fn name_clash_items(conn: &Connection, rules: &LinkerRules) -> Result<BTreeMap<String, Vec<DiagnosticItem>>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT name, kind, path, COALESCE(line_number, 0)
        FROM search_symbols
        WHERE is_class_like = 1
        ORDER BY name, kind, path
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    let mut groups: HashMap<String, Vec<NameClashRow>> = HashMap::new();
    for row in rows.filter_map(|item| item.ok()) {
        groups.entry(row.0).or_default().push(NameClashRow {
            kind: row.1,
            file_path: normalize_path(&row.2),
            line: row.3.max(0) as u32,
        });
    }

    let severity = DiagnosticSeverity::from(rules.name_clash_severity);
    let mut by_file = BTreeMap::new();
    for (name, rows) in groups {
        let kinds = rows.iter().map(|row| row.kind.as_str()).collect::<HashSet<_>>();
        if kinds.len() <= 1 {
            continue;
        }
        let related = rows
            .iter()
            .map(|row| DiagnosticRelatedItem {
                file_path: Some(row.file_path.clone()),
                line: row.line,
                character: 0,
                message: format!("{} ({})", short_file_name(&row.file_path), row.kind),
            })
            .collect::<Vec<_>>();
        let kinds_text = {
            let mut items = kinds.into_iter().collect::<Vec<_>>();
            items.sort();
            items.join(", ")
        };

        for row in rows {
            by_file
                .entry(row.file_path.clone())
                .or_insert_with(Vec::new)
                .push(
                    DiagnosticItem::new(
                        Some(&row.file_path),
                        row.line,
                        0,
                        severity.clone(),
                        SOURCE_NAME,
                        LNK_NAME_CLASH,
                        format!("Symbol '{}' is indexed as multiple kinds: {}.", name, kinds_text),
                    )
                    .with_end(row.line, 1)
                    .with_related(related.clone()),
                );
        }
    }
    Ok(by_file)
}

fn collect_class_symbols(file_path: &str, content: &str) -> Result<Vec<ClassSymbol>> {
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    let query = Query::new(&language, crate::scanner::QUERY_STR)?;
    let (classes, _, _) = cpp::parse_content(content, file_path, &language, &query)?;
    Ok(classes
        .into_iter()
        .filter(|class| class.decl_kind != "forward" && !class.is_synthetic_impl_scope)
        .filter(|_| !is_generated_header_path(file_path))
        .map(|class| build_class_symbol(file_path, class))
        .collect())
}

fn build_class_symbol(file_path: &str, class: ClassInfo) -> ClassSymbol {
    let fq_name = if let Some(namespace) = &class.namespace {
        format!("{}::{}", namespace, class.class_name)
    } else {
        class.class_name.clone()
    };

    let mut bases = class
        .base_classes
        .into_iter()
        .map(|base| cpp::clean_type_string(&base))
        .filter(|base| !base.is_empty())
        .collect::<Vec<_>>();
    bases.sort();

    let mut members = class
        .members
        .into_iter()
        .map(normalized_member_repr)
        .collect::<Vec<_>>();
    members.sort();

    let fingerprint = format!(
        "{}|bases={}|members={}",
        class.symbol_type,
        bases.join(";"),
        members.join(";")
    );

    ClassSymbol {
        file_path: normalize_path(file_path),
        line: class.line.saturating_sub(1) as u32,
        end_line: class.end_line.saturating_sub(1) as u32,
        fq_name,
        fingerprint,
    }
}

fn normalized_member_repr(member: MemberInfo) -> String {
    let kind = member.mem_type;
    let name = member.name;
    let access = member.access;
    let ty = member
        .return_type
        .as_deref()
        .map(cpp::clean_type_string)
        .unwrap_or_default();
    let detail = member
        .detail
        .as_deref()
        .map(normalize_parameter_signature)
        .unwrap_or_default();
    format!("{kind}|{name}|{ty}|{detail}|{access}")
}

fn collect_function_symbols(file_path: &str, content: &str) -> Result<Vec<FunctionSymbol>> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;
    let Some(tree) = parser.parse(content, None) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    walk_function_nodes(tree.root_node(), content, file_path, &mut out);
    Ok(out)
}

fn walk_function_nodes(node: Node, content: &str, file_path: &str, out: &mut Vec<FunctionSymbol>) {
    if let Some(symbol) = function_symbol_from_node(node, content, file_path) {
        out.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_function_nodes(child, content, file_path, out);
    }
}

fn function_symbol_from_node(node: Node, content: &str, file_path: &str) -> Option<FunctionSymbol> {
    let kind = node.kind();
    if !matches!(
        kind,
        "function_definition" | "declaration" | "field_declaration" | "unreal_function_declaration"
    ) {
        return None;
    }

    let text = node_text(node, content).trim().to_string();
    if text.is_empty() || is_macro_invocation_statement(&text) {
        return None;
    }

    let declarator = find_function_declarator(node)?;
    let identity = resolve_function_identity(declarator, content)?;
    let parameters = find_child_by_kind(declarator, "parameter_list")
        .map(|params| node_text(params, content).to_string())
        .unwrap_or_default();
    if parameters.is_empty() {
        return None;
    }

    let owner = identity
        .owner_name
        .clone()
        .or_else(|| find_enclosing_class_name(node, content));
    let name = identity.name.trim().to_string();
    if name.is_empty() {
        return None;
    }

    let return_type = extract_return_type(node, declarator, content);
    let owner_short = owner.as_deref().map(short_name).unwrap_or("");
    let cleaned_return = if name == owner_short || name == format!("~{}", owner_short) {
        String::new()
    } else {
        cpp::clean_type_string(&return_type)
    };
    let signature = build_function_signature(
        owner.as_deref(),
        &name,
        &parameters,
        &cleaned_return,
        &text,
        contains_token(&text, "const"),
    );

    let start = node.start_position();
    let end = node.end_position();
    Some(FunctionSymbol {
        file_path: normalize_path(file_path),
        line: identity.line.saturating_sub(1) as u32,
        character: start.column as u32,
        end_line: end.row as u32,
        end_character: end.column as u32,
        owner_name: owner,
        name,
        signature,
        is_definition: kind == "function_definition",
        is_virtual: contains_token(&text, "virtual"),
        is_template: has_enclosing_template(node),
        is_inline: contains_token(&text, "inline") || contains_token(&text, "FORCEINLINE"),
        is_static: contains_token(&text, "static"),
        is_pure_virtual: text.contains("= 0"),
        is_ue_reflected: text.contains("UFUNCTION") || text.contains("UDELEGATE"),
        generated_header: is_generated_header_path(file_path),
    })
}

struct FunctionIdentity {
    name: String,
    owner_name: Option<String>,
    line: usize,
}

fn resolve_function_identity(declarator: Node, content: &str) -> Option<FunctionIdentity> {
    let mut current = declarator;
    loop {
        match current.kind() {
            "identifier" | "field_identifier" => {
                return Some(FunctionIdentity {
                    name: node_text(current, content).to_string(),
                    owner_name: None,
                    line: current.start_position().row + 1,
                });
            }
            "qualified_identifier" => {
                let owner_name = current
                    .child_by_field_name("scope")
                    .map(|scope| node_text(scope, content).trim().to_string());
                let name = current
                    .child_by_field_name("name")
                    .map(|name| node_text(name, content).trim().to_string())?;
                return Some(FunctionIdentity {
                    name,
                    owner_name,
                    line: current.start_position().row + 1,
                });
            }
            "function_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator" => {
                current = current.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
}

fn build_function_signature(
    owner: Option<&str>,
    name: &str,
    parameters: &str,
    return_type: &str,
    full_text: &str,
    is_const: bool,
) -> String {
    let params = normalize_parameter_signature(parameters);
    let mut signature = if let Some(owner) = owner.filter(|owner| !owner.is_empty()) {
        if return_type.is_empty() {
            format!("{owner}::{name}{params}")
        } else {
            format!("{return_type} {owner}::{name}{params}")
        }
    } else if return_type.is_empty() {
        format!("{name}{params}")
    } else {
        format!("{return_type} {name}{params}")
    };

    let suffix = definition_suffix(full_text, parameters, is_const);
    if !suffix.is_empty() {
        signature.push(' ');
        signature.push_str(&suffix);
    }
    signature
}

fn definition_suffix(full_text: &str, parameters: &str, is_const: bool) -> String {
    let mut suffixes = Vec::new();
    let params_end = full_text
        .find(parameters)
        .map(|start| start + parameters.len());
    let trailing = params_end
        .and_then(|start| full_text.get(start..))
        .unwrap_or("");

    if is_const {
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

    suffixes.join(" ")
}

fn extract_noexcept_text(trailing: &str) -> Option<String> {
    let index = trailing.find("noexcept")?;
    let rest = trailing.get(index..)?.trim_start();
    if let Some(paren_start) = rest.find('(') {
        let mut depth = 0i32;
        for (idx, ch) in rest.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && idx >= paren_start {
                        return Some(rest[..=idx].trim().to_string());
                    }
                }
                _ => {}
            }
        }
    }
    Some("noexcept".to_string())
}

fn extract_return_type(node: Node, declarator: Node, content: &str) -> String {
    if let Some(type_node) = node.child_by_field_name("type") {
        return node_text(type_node, content).to_string();
    }

    let start = node.start_byte();
    let end = declarator.start_byte();
    if end <= start {
        return String::new();
    }

    let raw = content.get(start..end).unwrap_or("");
    if let Some(close_paren) = raw.rfind(')') {
        cpp::clean_type_string(&raw[close_paren + 1..])
    } else {
        cpp::clean_type_string(raw)
    }
}

fn should_ignore_missing_definition(
    function: &FunctionSymbol,
    class_macro_flags: &HashMap<String, bool>,
    rules: &LinkerRules,
) -> bool {
    if rules
        .missing_def_ignore_name_prefixes
        .iter()
        .any(|prefix| matches_name_ignore_prefix(&function.name, prefix))
    {
        return true;
    }

    if let Some(owner) = function.owner_name.as_deref() {
        let owner_short = short_name(owner);
        if rules
            .missing_def_ignore_class_prefixes
            .iter()
            .any(|prefix| owner_short.starts_with(prefix))
        {
            return true;
        }
        if rules.missing_def_check_only_non_ue_reflected
            && class_macro_flags.get(owner).copied().unwrap_or(false)
        {
            return true;
        }
    }

    false
}

fn matches_name_ignore_prefix(name: &str, prefix: &str) -> bool {
    if !name.starts_with(prefix) {
        return false;
    }

    let Some(rest) = name.get(prefix.len()..) else {
        return true;
    };
    if rest.is_empty() {
        return true;
    }

    if prefix.ends_with('_') {
        return true;
    }

    rest.chars()
        .next()
        .map(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        .unwrap_or(true)
}

fn read_cache_if_fresh(cache_path: &Path, primary_db_path: &str) -> Result<Option<LinkerCacheFile>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let cache: LinkerCacheFile = serde_json::from_str(
        &fs::read_to_string(cache_path)
            .with_context(|| format!("failed to read linker cache {}", cache_path.display()))?,
    )
    .with_context(|| format!("failed to parse linker cache {}", cache_path.display()))?;
    if cache.db_mtime_secs != db_mtime_secs(primary_db_path).unwrap_or_default() {
        return Ok(None);
    }
    Ok(Some(cache))
}

fn write_cache(path: &Path, cache: &LinkerCacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create linker cache dir {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(cache)?)
        .with_context(|| format!("failed to write linker cache {}", path.display()))?;
    Ok(())
}

fn linker_cache_path(project_root: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ucore")
        .join("linker_diags.json")
}

fn db_mtime_secs(primary_db_path: &str) -> Result<u64> {
    Ok(fs::metadata(primary_db_path)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn runtime_item_to_cached(item: DiagnosticItem) -> CachedDiagnosticItem {
    CachedDiagnosticItem {
        file_path: item.file_path,
        line: item.line,
        character: item.character,
        end_line: item.end_line,
        end_character: item.end_character,
        severity: item.severity,
        code: item.code.to_string(),
        message: item.message,
        related: item
            .related
            .into_iter()
            .map(|related| CachedDiagnosticRelatedItem {
                file_path: related.file_path,
                line: related.line,
                character: related.character,
                message: related.message,
            })
            .collect(),
    }
}

fn cached_item_to_runtime(item: CachedDiagnosticItem) -> DiagnosticItem {
    DiagnosticItem::new(
        item.file_path.as_deref(),
        item.line,
        item.character,
        item.severity,
        SOURCE_NAME,
        linker_code_ref(&item.code),
        item.message,
    )
    .with_end(item.end_line, item.end_character)
    .with_related(
        item.related
            .into_iter()
            .map(|related| DiagnosticRelatedItem {
                file_path: related.file_path,
                line: related.line,
                character: related.character,
                message: related.message,
            })
            .collect(),
    )
}

fn linker_code_ref(code: &str) -> &'static str {
    match code {
        LNK_ODR_FUNCTION => LNK_ODR_FUNCTION,
        LNK_ODR_CLASS => LNK_ODR_CLASS,
        LNK_MISSING_DEF => LNK_MISSING_DEF,
        LNK_NAME_CLASH => LNK_NAME_CLASH,
        _ => LNK_ODR_FUNCTION,
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn short_file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn short_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn display_function_name(function: &FunctionSymbol) -> String {
    if let Some(owner) = function.owner_name.as_deref().filter(|owner| !owner.is_empty()) {
        format!("{}::{}", owner, function.name)
    } else {
        function.name.clone()
    }
}

fn is_header_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".h") || lower.ends_with(".hh") || lower.ends_with(".hpp") || lower.ends_with(".hxx")
}

fn is_generated_header_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".generated.h")
}

fn contains_token(text: &str, token: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|part| part == token)
}

fn has_enclosing_template(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(node) = current {
        if node.kind() == "template_declaration" {
            return true;
        }
        current = node.parent();
    }
    false
}

fn node_text<'a>(node: Node<'a>, content: &'a str) -> &'a str {
    node.utf8_text(content.as_bytes()).unwrap_or("")
}

fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
        if let Some(found) = find_child_by_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn find_function_declarator(node: Node) -> Option<Node> {
    if node.kind() == "function_declarator" {
        return Some(node);
    }
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if let Some(found) = find_function_declarator(declarator) {
            return Some(found);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_function_declarator(child) {
            return Some(found);
        }
    }
    None
}

fn find_enclosing_class_name(node: Node, content: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_specifier"
                | "struct_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
        ) {
            let name = parent.child_by_field_name("name")?;
            return Some(node_text(name, content).trim().to_string());
        }
        current = parent.parent();
    }
    None
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
    let Some(first) = prefix.chars().next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    prefix
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
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
            '=' if paren_depth > 0 => skipping_default = true,
            _ => out.push(ch),
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rusqlite::Connection;

    use crate::db;
    use crate::types::{ParseData, ParseResult, ProgressReporter};

    struct NoopReporter;

    impl ProgressReporter for NoopReporter {
        fn report(&self, _stage: &str, _current: usize, _total: usize, _message: &str) {}
        fn report_plan(&self, _phases: &[crate::types::PhaseInfo]) {}
    }

    fn temp_root(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "ucore-linker-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("Source/Game/Public")).unwrap();
        fs::create_dir_all(base.join("Source/Game/Private")).unwrap();
        base
    }

    fn parse_file(path: &Path, text: &str) -> ParseResult {
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = Query::new(&language, crate::scanner::QUERY_STR).unwrap();
        let (classes, calls, includes) =
            cpp::parse_content(text, path.to_string_lossy().as_ref(), &language, &query).unwrap();
        ParseResult {
            path: normalize_path(path.to_string_lossy().as_ref()),
            status: "parsed".to_string(),
            mtime: 1,
            data: Some(ParseData {
                classes,
                calls,
                includes,
                gameplay_tags: Vec::new(),
                macro_definitions: Vec::new(),
                parser: "tree-sitter".to_string(),
                new_hash: "hash".to_string(),
            }),
            module_id: None,
        }
    }

    #[test]
    fn full_rebuild_reports_duplicate_function_and_missing_definition() {
        let root = temp_root("dup-func");
        let header = root.join("Source/Game/Public/A.h");
        let a_cpp = root.join("Source/Game/Private/A.cpp");
        let b_cpp = root.join("Source/Game/Private/B.cpp");
        fs::write(&header, "#pragma once\nvoid OnlyDeclared();\nvoid DupFunc();\n").unwrap();
        fs::write(&a_cpp, "#include \"A.h\"\nvoid DupFunc() {}\n").unwrap();
        fs::write(&b_cpp, "#include \"A.h\"\nvoid DupFunc() {}\n").unwrap();

        let db_path = root.join("project.db");
        let mut conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        db::save_to_db_incremental(
            &mut conn,
            &[
                parse_file(&header, &fs::read_to_string(&header).unwrap()),
                parse_file(&a_cpp, &fs::read_to_string(&a_cpp).unwrap()),
                parse_file(&b_cpp, &fs::read_to_string(&b_cpp).unwrap()),
            ],
            Arc::new(NoopReporter),
        )
        .unwrap();

        let mut rules = LinkerRules::default();
        rules.check_missing_def = true;
        rebuild_full_cache(&conn, root.to_string_lossy().as_ref(), db_path.to_string_lossy().as_ref(), &rules)
            .unwrap();

        let a_items = cached_items_for_file(
            root.to_string_lossy().as_ref(),
            db_path.to_string_lossy().as_ref(),
            Some(a_cpp.to_string_lossy().as_ref()),
            &rules,
        )
        .unwrap();
        assert!(a_items.iter().any(|item| item.code == LNK_ODR_FUNCTION));

        let h_items = cached_items_for_file(
            root.to_string_lossy().as_ref(),
            db_path.to_string_lossy().as_ref(),
            Some(header.to_string_lossy().as_ref()),
            &rules,
        )
        .unwrap();
        assert!(h_items.iter().any(|item| item.code == LNK_MISSING_DEF));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn full_rebuild_reports_class_odr_conflict() {
        let root = temp_root("class-odr");
        let a_h = root.join("Source/Game/Public/C_odr_a.h");
        let b_h = root.join("Source/Game/Public/C_odr_b.h");
        fs::write(&a_h, "class UBar { int x = 1; };\n").unwrap();
        fs::write(&b_h, "class UBar { float y = 2.0f; };\n").unwrap();

        let db_path = root.join("project.db");
        let mut conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        db::save_to_db_incremental(
            &mut conn,
            &[
                parse_file(&a_h, &fs::read_to_string(&a_h).unwrap()),
                parse_file(&b_h, &fs::read_to_string(&b_h).unwrap()),
            ],
            Arc::new(NoopReporter),
        )
        .unwrap();

        let rules = LinkerRules::default();
        rebuild_full_cache(&conn, root.to_string_lossy().as_ref(), db_path.to_string_lossy().as_ref(), &rules)
            .unwrap();
        let items = cached_items_for_file(
            root.to_string_lossy().as_ref(),
            db_path.to_string_lossy().as_ref(),
            Some(a_h.to_string_lossy().as_ref()),
            &rules,
        )
        .unwrap();
        assert!(items.iter().any(|item| item.code == LNK_ODR_CLASS));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_definition_ignore_prefix_keeps_only_prefix_boundary_matches() {
        assert!(matches_name_ignore_prefix("OnClicked", "On"));
        assert!(matches_name_ignore_prefix("HandleEvent", "Handle"));
        assert!(matches_name_ignore_prefix("BP_Test", "BP_"));
        assert!(!matches_name_ignore_prefix("OnlyDeclared", "On"));
        assert!(!matches_name_ignore_prefix("Handler", "Handle"));
    }
}
