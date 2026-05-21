use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tree_sitter::{Node, Parser, Point};

use crate::db::ensure_search_projections;
use crate::db::text;
use crate::query::goto;

const MAX_RESULTS: usize = 300;
const MAX_FILES: usize = 2000;
const SQL_CHUNK_SIZE: usize = 100;
const STREAM_BATCH_SIZE: usize = 15;

#[derive(Clone, Serialize, Deserialize)]
struct MemberDeclHotEntry {
    path: String,
    line: i64,
    class_name: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CallSiteHotEntry {
    file_id: i64,
    path: String,
    line: i64,
}

#[derive(Serialize, Deserialize)]
pub struct UsageHotIndex {
    file_paths_by_id: HashMap<i64, String>,
    file_ids_by_path: HashMap<String, i64>,
    all_file_ids: Vec<i64>,
    defs_by_name: HashMap<String, Vec<i64>>,
    calls_by_name_lc: HashMap<String, Vec<CallSiteHotEntry>>,
    include_reverse: HashMap<i64, Vec<i64>>,
    member_defs_by_key: HashMap<String, Vec<i64>>,
    member_decls_by_key: HashMap<String, Vec<MemberDeclHotEntry>>,
    class_like_names: HashSet<String>,
}

/// Find symbol usages and return all collected results at once.
/// 查找 symbol 使用位置，并一次性返回结果。
pub fn find_symbol_usages(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
) -> Result<Value> {
    ensure_search_projections(conn)?;
    find_symbol_usages_inner(conn, None, symbol_name, current_file, None)
}

/// Find symbol usages with cursor context for member-aware filtering.
/// 带光标上下文查找引用，用于更精确地区分类成员和同名局部变量。
pub fn find_symbol_usages_for_cursor(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
    content: Option<&str>,
    line: Option<u32>,
    character: Option<u32>,
) -> Result<Value> {
    ensure_search_projections(conn)?;
    let scope = match (content, line, character) {
        (Some(content), Some(line), Some(character)) => {
            resolve_usage_scope(conn, symbol_name, content, line, character)?
        }
        _ => None,
    };

    find_symbol_usages_inner(conn, None, symbol_name, current_file, scope.as_ref())
}

pub fn build_usage_hot_index(conn: &Connection) -> Result<UsageHotIndex> {
    ensure_search_projections(conn)?;

    let mut file_paths_by_id = HashMap::new();
    let mut file_ids_by_path = HashMap::new();
    let mut all_file_ids = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT file_id, path FROM search_files ORDER BY file_id"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, normalize_path(&row.get::<_, String>(1)?)))
    })?;
    for row in rows {
        let (file_id, path) = row?;
        file_ids_by_path.insert(path.clone(), file_id);
        file_paths_by_id.insert(file_id, path);
        all_file_ids.push(file_id);
    }

    let mut defs_by_name = HashMap::<String, Vec<i64>>::new();
    let mut calls_by_name_lc = HashMap::<String, Vec<CallSiteHotEntry>>::new();
    let mut include_reverse = HashMap::<i64, Vec<i64>>::new();
    let mut member_defs_by_key = HashMap::<String, Vec<i64>>::new();
    let mut member_decls_by_key = HashMap::<String, Vec<MemberDeclHotEntry>>::new();
    let mut class_like_names = HashSet::<String>::new();

    let mut stmt = conn.prepare(
        r#"
        SELECT name, file_id, owner_name, path, line_number, is_class_like
        FROM search_symbols
        ORDER BY file_id, line_number
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            normalize_path(&row.get::<_, String>(3)?),
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, i64>(5)? != 0,
        ))
    })?;
    for row in rows {
        let (name, file_id, owner_name, path, line_number, is_class_like) = row?;
        defs_by_name.entry(name.clone()).or_default().push(file_id);
        if is_class_like {
            class_like_names.insert(name);
        } else if let Some(owner_name) = owner_name {
            let key = member_key(&name, &owner_name);
            member_defs_by_key.entry(key.clone()).or_default().push(file_id);
            if let Some(line) = line_number {
                member_decls_by_key
                    .entry(key)
                    .or_default()
                    .push(MemberDeclHotEntry {
                        path,
                        line,
                        class_name: owner_name,
                    });
            }
        }
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT se.text, ev.file_id
        FROM enum_values ev
        JOIN strings se ON ev.name_id = se.id
        WHERE ev.file_id IS NOT NULL
        "#,
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    for row in rows {
        let (name, file_id) = row?;
        defs_by_name.entry(name).or_default().push(file_id);
    }

    let mut stmt = conn.prepare(
        "SELECT name_lc, file_id, path, line_number
         FROM search_symbol_calls
         ORDER BY file_id, line_number",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            CallSiteHotEntry {
                file_id: row.get::<_, i64>(1)?,
                path: normalize_path(&row.get::<_, String>(2)?),
                line: row.get::<_, i64>(3)?,
            },
        ))
    })?;
    for row in rows {
        let (name_lc, item) = row?;
        calls_by_name_lc.entry(name_lc).or_default().push(item);
    }

    let mut stmt = conn.prepare(
        "SELECT resolved_file_id, file_id FROM file_includes WHERE resolved_file_id IS NOT NULL"
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    for row in rows {
        let (resolved_id, file_id) = row?;
        include_reverse.entry(resolved_id).or_default().push(file_id);
    }

    dedupe_id_map(&mut defs_by_name);
    dedupe_call_site_map(&mut calls_by_name_lc);
    dedupe_i64_id_map(&mut include_reverse);
    dedupe_id_map(&mut member_defs_by_key);
    for items in member_decls_by_key.values_mut() {
        items.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.class_name.cmp(&right.class_name))
        });
        items.dedup_by(|left, right| {
            left.path == right.path && left.line == right.line && left.class_name == right.class_name
        });
    }

    Ok(UsageHotIndex {
        file_paths_by_id,
        file_ids_by_path,
        all_file_ids,
        defs_by_name,
        calls_by_name_lc,
        include_reverse,
        member_defs_by_key,
        member_decls_by_key,
        class_like_names,
    })
}

pub fn find_symbol_usages_for_cursor_with_hot_index(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    current_file: Option<&str>,
    content: Option<&str>,
    line: Option<u32>,
    character: Option<u32>,
) -> Result<Value> {
    ensure_search_projections(conn)?;
    let scope = match (content, line, character) {
        (Some(content), Some(line), Some(character)) => {
            resolve_usage_scope(conn, symbol_name, content, line, character)?
        }
        _ => None,
    };

    find_symbol_usages_inner(conn, hot_index, symbol_name, current_file, scope.as_ref())
}

fn find_symbol_usages_inner(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    current_file: Option<&str>,
    scope: Option<&UsageScope>,
) -> Result<Value> {
    let symbol_name = symbol_name.trim();

    if symbol_name.is_empty() {
        return Ok(json!({
            "results": [],
            "searched_files": 0,
            "found_definition": false,
        }));
    }

    let candidates = collect_candidate_files(conn, hot_index, symbol_name, current_file, scope)?;
    let mut results = match scope {
        Some(UsageScope::Member(member_scope)) => get_member_declaration_results(
            conn,
            hot_index,
            symbol_name,
            &member_scope.member_owner_class,
        )?,
        _ => Vec::new(),
    };

    if !matches!(scope, Some(UsageScope::Local(_))) {
        let direct_results = get_direct_call_results(
            conn,
            hot_index,
            symbol_name,
            scope,
            MAX_RESULTS.saturating_sub(results.len()),
        )?;
        for item in direct_results {
            push_unique_result(&mut results, item);
            if results.len() >= MAX_RESULTS {
                break;
            }
        }
    }

    let indexed_search_used = if results.len() >= MAX_RESULTS {
        false
    } else {
        search_indexed_candidate_lines(
            conn,
            &candidates.file_paths,
            symbol_name,
            MAX_RESULTS.saturating_sub(results.len()),
            scope,
            |item| {
                push_unique_result(&mut results, item);
                Ok(())
            },
        )?
    };

    if !indexed_search_used {
        for path in &candidates.file_paths {
            if results.len() >= MAX_RESULTS {
                break;
            }

            search_in_file(conn, path, symbol_name, MAX_RESULTS - results.len(), scope, |item| {
                push_unique_result(&mut results, item);
                Ok(())
            })?;
        }
    }

    Ok(json!({
        "results": results,
        "searched_files": candidates.file_paths.len(),
        "found_definition": candidates.found_definition,
        "scope": scope.map(UsageScope::name).unwrap_or("unresolved"),
    }))
}

#[derive(Debug, Clone)]
enum UsageScope {
    Local(LocalScope),
    Member(MemberScope),
}

#[derive(Debug, Clone)]
struct LocalScope {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone)]
struct MemberScope {
    member_owner_class: String,
    context_class: Option<String>,
}

impl MemberScope {
    fn candidate_classes(&self) -> Vec<&str> {
        let mut classes = vec![self.member_owner_class.as_str()];

        if let Some(context_class) = self.context_class.as_deref() {
            if context_class != self.member_owner_class {
                classes.push(context_class);
            }
        }

        classes
    }
}

impl UsageScope {
    fn name(&self) -> &'static str {
        match self {
            UsageScope::Local(_) => "local",
            UsageScope::Member(_) => "member",
        }
    }
}

/// Resolve whether the cursor target is a class member.
/// 判断当前光标目标是否是类成员。
fn resolve_usage_scope(
    conn: &Connection,
    fallback_symbol: &str,
    content: &str,
    line: u32,
    character: u32,
) -> Result<Option<UsageScope>> {
    let Some(ctx) = goto::extract_cursor_context(content, line, character) else {
        return Ok(None);
    };

    let symbol = if ctx.symbol.trim().is_empty() {
        fallback_symbol.trim()
    } else {
        ctx.symbol.trim()
    };

    if symbol.is_empty() {
        return Ok(None);
    }

    if let Some(local_scope) = resolve_local_scope(content, symbol, line, character) {
        return Ok(Some(UsageScope::Local(local_scope)));
    }

    let target_class = match ctx.qualifier.as_deref() {
        Some("this") => ctx.enclosing_class.clone(),
        Some("Super") if ctx.qualifier_op.as_deref() == Some("::") => ctx.enclosing_class.clone(),
        Some(qualifier) if matches!(ctx.qualifier_op.as_deref(), Some(".") | Some("->")) => {
            goto::infer_var_type(content, qualifier, Some(line)).or_else(|| Some(qualifier.to_string()))
        }
        Some(qualifier) if ctx.qualifier_op.as_deref() == Some("::") => {
            Some(qualifier.to_string())
        }
        _ => ctx.enclosing_class.clone(),
    };

    let Some(target_class) = target_class else {
        return Ok(None);
    };

    let Some(member) =
        goto::find_symbol_in_inheritance_chain(conn, None, &target_class, symbol)?
    else {
        return Ok(None);
    };

    let Some(member_owner_class) = member.get("class_name").and_then(Value::as_str) else {
        return Ok(None);
    };

    Ok(Some(UsageScope::Member(MemberScope {
        member_owner_class: member_owner_class.to_string(),
        context_class: ctx.enclosing_class,
    })))
}

/// Resolve a local variable or parameter scope inside the current function.
/// 解析当前函数内的局部变量或参数作用域。
fn resolve_local_scope(
    content: &str,
    symbol_name: &str,
    line: u32,
    character: u32,
) -> Option<LocalScope> {
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;

    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let row = line as usize;
    let col = character as usize;
    let current = Point::new(row, col);
    let next = Point::new(row, col.saturating_add(1));
    let raw_node = root
        .descendant_for_point_range(current, next)
        .or_else(|| {
            let previous = Point::new(row, col.saturating_sub(1));
            root.descendant_for_point_range(previous, current)
        })
        .or_else(|| root.descendant_for_point_range(current, current))?;
    let function = enclosing_function(raw_node)?;

    if has_local_declaration(function, content.as_bytes(), symbol_name, line as usize) {
        return Some(LocalScope {
            start_line: function.start_position().row + 1,
            end_line: function.end_position().row + 1,
        });
    }

    None
}

fn enclosing_function<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let mut current = Some(node);

    while let Some(node) = current {
        if matches!(
            node.kind(),
            "function_definition" | "unreal_function_definition" | "lambda_expression"
        ) {
            return Some(node);
        }

        current = node.parent();
    }

    None
}

fn has_local_declaration(
    node: Node,
    src: &[u8],
    symbol_name: &str,
    cursor_row: usize,
) -> bool {
    if node.start_position().row > cursor_row {
        return false;
    }

    if matches!(node.kind(), "declaration" | "parameter_declaration") {
        if declaration_names(node, src)
            .into_iter()
            .any(|name| name == symbol_name)
        {
            return true;
        }
    }

    for child in children_of(node) {
        if has_local_declaration(child, src, symbol_name, cursor_row) {
            return true;
        }
    }

    false
}

fn declaration_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    if let Some(declarator) = node.child_by_field_name("declarator") {
        collect_decl_names(declarator, src, &mut names);
    }

    names
}

fn collect_decl_names(node: Node, src: &[u8], names: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "field_identifier" => {
            let name = node_text(node, src).trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }

        "function_declarator" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                collect_decl_names(declarator, src, names);
            }
        }

        "init_declarator" | "pointer_declarator" | "reference_declarator" | "array_declarator" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                collect_decl_names(declarator, src, names);
            }
        }

        _ => {
            for child in children_of(node) {
                collect_decl_names(child, src, names);
            }
        }
    }
}

fn node_text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn children_of<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Find symbol usages and stream results in small batches.
/// 查找 symbol 使用位置，并以小批次流式返回结果。
pub fn find_symbol_usages_async<F>(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
    mut on_items: F,
) -> Result<Value>
where
    F: FnMut(Vec<Value>) -> Result<()>,
{
    let symbol_name = symbol_name.trim();

    if symbol_name.is_empty() {
        return Ok(json!({
            "searched_files": 0,
            "found_definition": false,
            "total_results": 0,
        }));
    }

    let candidates = collect_candidate_files(conn, None, symbol_name, current_file, None)?;
    let mut total_results = 0usize;
    let mut batch = Vec::new();

    let indexed_search_used = search_indexed_candidate_lines(
        conn,
        &candidates.file_paths,
        symbol_name,
        MAX_RESULTS.saturating_sub(total_results),
        None,
        |item| {
            batch.push(item);
            total_results += 1;

            if batch.len() >= STREAM_BATCH_SIZE {
                on_items(std::mem::take(&mut batch))?;
            }

            Ok(())
        },
    )?;

    if !indexed_search_used {
        for path in &candidates.file_paths {
            if total_results >= MAX_RESULTS {
                break;
            }

            search_in_file(conn, path, symbol_name, MAX_RESULTS - total_results, None, |item| {
                batch.push(item);
                total_results += 1;

                if batch.len() >= STREAM_BATCH_SIZE {
                    on_items(std::mem::take(&mut batch))?;
                }

                Ok(())
            })?;
        }
    }

    if !batch.is_empty() {
        on_items(batch)?;
    }

    Ok(json!({
        "searched_files": candidates.file_paths.len(),
        "found_definition": candidates.found_definition,
        "total_results": total_results,
    }))
}

// -----------------------------------------------------------------------------
// Candidate collection
// -----------------------------------------------------------------------------

struct CandidateFiles {
    file_paths: Vec<String>,
    found_definition: bool,
}

/// Collect exact-scope files where the symbol may be used.
/// 只收集精确作用域内 symbol 可能出现的候选文件。
fn collect_candidate_files(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    current_file: Option<&str>,
    scope: Option<&UsageScope>,
) -> Result<CandidateFiles> {
    if let Some(UsageScope::Local(_)) = scope {
        let file_paths = current_file
            .map(|path| vec![normalize_path(path)])
            .unwrap_or_default();

        return Ok(CandidateFiles {
            file_paths,
            found_definition: true,
        });
    }

    let Some(UsageScope::Member(member_scope)) = scope else {
        return collect_unresolved_candidate_files(conn, hot_index, symbol_name, current_file);
    };

    let def_ids = hot_index
        .and_then(|index| index.find_member_definition_file_ids(symbol_name, &member_scope.member_owner_class))
        .unwrap_or(find_member_definition_file_ids(conn, symbol_name, &member_scope.member_owner_class)?);
    let found_definition = !def_ids.is_empty();

    let mut candidate_ids = HashSet::new();

    for id in &def_ids {
        candidate_ids.insert(*id);
    }

    let include_ids = hot_index
        .and_then(|index| index.find_including_file_ids(&def_ids))
        .unwrap_or(find_including_file_ids(conn, &def_ids)?);
    for id in include_ids {
        candidate_ids.insert(id);
    }

    if let Some(current) = current_file {
        let current_id = hot_index
            .and_then(|index| index.find_file_id(current))
            .or(find_file_id(conn, current)?);
        if let Some(id) = current_id {
            candidate_ids.insert(id);
        }
    }

    let mut ids = candidate_ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(MAX_FILES);

    let mut file_paths = get_file_paths_by_ids(conn, &ids)?;
    file_paths.sort();
    file_paths.dedup();

    Ok(CandidateFiles {
        file_paths,
        found_definition,
    })
}

/// Collect fallback candidate files when the cursor scope cannot be resolved.
/// 当无法解析光标作用域时，回退收集一个可搜索的候选文件集合。
fn collect_unresolved_candidate_files(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    current_file: Option<&str>,
) -> Result<CandidateFiles> {
    let is_type_like = hot_index
        .map(|index| index.is_type_like_symbol(symbol_name))
        .unwrap_or(is_type_like_symbol(conn, symbol_name)?);
    if is_type_like {
        return collect_type_candidate_files(conn, hot_index, symbol_name, current_file);
    }

    let mut candidate_ids = HashSet::new();
    let mut found_definition = false;

    let definition_ids = hot_index
        .and_then(|index| index.find_symbol_definition_file_ids(symbol_name))
        .unwrap_or(find_symbol_definition_file_ids(conn, symbol_name)?);
    for id in definition_ids {
        found_definition = true;
        candidate_ids.insert(id);
    }

    let call_ids = hot_index
        .and_then(|index| index.find_symbol_call_file_ids(symbol_name))
        .unwrap_or(find_symbol_call_file_ids(conn, symbol_name)?);
    for id in call_ids {
        candidate_ids.insert(id);
    }

    let seed_ids = candidate_ids.iter().copied().collect::<Vec<_>>();
    let include_ids = hot_index
        .and_then(|index| index.find_including_file_ids(&seed_ids))
        .unwrap_or(find_including_file_ids(conn, &seed_ids)?);
    for id in include_ids {
        candidate_ids.insert(id);
    }

    if let Some(current) = current_file {
        let current_id = hot_index
            .and_then(|index| index.find_file_id(current))
            .or(find_file_id(conn, current)?);
        if let Some(id) = current_id {
            candidate_ids.insert(id);
        }
    }

    if candidate_ids.is_empty() {
        let all_file_ids = hot_index
            .map(|index| index.collect_all_file_ids())
            .unwrap_or(collect_all_file_ids(conn)?);
        for id in all_file_ids {
            candidate_ids.insert(id);
            if candidate_ids.len() >= MAX_FILES {
                break;
            }
        }
    }

    let mut ids = candidate_ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(MAX_FILES);

    let mut file_paths = hot_index
        .and_then(|index| index.get_file_paths_by_ids(&ids))
        .unwrap_or(get_file_paths_by_ids(conn, &ids)?);
    file_paths.sort();
    file_paths.dedup();

    Ok(CandidateFiles {
        file_paths,
        found_definition,
    })
}

fn collect_type_candidate_files(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    current_file: Option<&str>,
) -> Result<CandidateFiles> {
    let mut candidate_ids = HashSet::new();
    let mut found_definition = false;

    let definition_ids = hot_index
        .and_then(|index| index.find_symbol_definition_file_ids(symbol_name))
        .unwrap_or(find_symbol_definition_file_ids(conn, symbol_name)?);
    for id in definition_ids {
        found_definition = true;
        candidate_ids.insert(id);
    }

    let type_ref_ids = hot_index
        .and_then(|index| index.find_type_reference_file_ids(symbol_name))
        .unwrap_or(find_type_reference_file_ids(conn, symbol_name)?);
    for id in type_ref_ids {
        candidate_ids.insert(id);
    }

    if let Some(current) = current_file {
        let current_id = hot_index
            .and_then(|index| index.find_file_id(current))
            .or(find_file_id(conn, current)?);
        if let Some(id) = current_id {
            candidate_ids.insert(id);
        }
    }

    let mut ids = candidate_ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(MAX_FILES);

    let mut file_paths = hot_index
        .and_then(|index| index.get_file_paths_by_ids(&ids))
        .unwrap_or(get_file_paths_by_ids(conn, &ids)?);
    file_paths.sort();
    file_paths.dedup();

    Ok(CandidateFiles {
        file_paths,
        found_definition,
    })
}

fn is_type_like_symbol(conn: &Connection, symbol_name: &str) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM search_symbols WHERE is_class_like = 1 AND name = ? LIMIT 1",
            [symbol_name],
            |_| Ok(()),
        )
        .optional()?
        .is_some();

    Ok(exists)
}

impl UsageHotIndex {
    pub fn size_hint(&self) -> usize {
        self.file_paths_by_id.len()
            + self.file_ids_by_path.len()
            + self.all_file_ids.len()
            + self.defs_by_name.len()
            + self.calls_by_name_lc.len()
            + self.include_reverse.len()
            + self.member_defs_by_key.len()
            + self.member_decls_by_key.len()
            + self.class_like_names.len()
    }

    fn find_member_definition_file_ids(&self, symbol_name: &str, owner_class: &str) -> Option<Vec<i64>> {
        self.member_defs_by_key.get(&member_key(symbol_name, owner_class)).cloned()
    }

    fn find_symbol_definition_file_ids(&self, symbol_name: &str) -> Option<Vec<i64>> {
        self.defs_by_name.get(symbol_name).cloned()
    }

    fn find_symbol_call_file_ids(&self, symbol_name: &str) -> Option<Vec<i64>> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for item in self.calls_by_name_lc.get(&symbol_name.to_ascii_lowercase())? {
            if seen.insert(item.file_id) {
                ids.push(item.file_id);
            }
        }
        Some(ids)
    }

    fn get_direct_call_entries(&self, symbol_name: &str, limit: usize) -> Option<Vec<CallSiteHotEntry>> {
        let mut results = Vec::new();
        for entry in self.calls_by_name_lc.get(&symbol_name.to_ascii_lowercase())? {
            if results.len() >= limit {
                break;
            }
            results.push(entry.clone());
        }
        Some(results)
    }

    fn find_including_file_ids(&self, def_ids: &[i64]) -> Option<Vec<i64>> {
        if def_ids.is_empty() {
            return Some(Vec::new());
        }

        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for def_id in def_ids {
            if let Some(include_ids) = self.include_reverse.get(def_id) {
                for &id in include_ids {
                    if seen.insert(id) {
                        ids.push(id);
                    }
                }
            }
        }
        Some(ids)
    }

    fn find_file_id(&self, file_path: &str) -> Option<i64> {
        let normalized = normalize_path(file_path);
        self.file_ids_by_path.get(&normalized).copied().or_else(|| {
            let filename = Path::new(file_path).file_name()?.to_str()?;
            self.file_paths_by_id.iter().find_map(|(file_id, path)| {
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| name.eq_ignore_ascii_case(filename))
                    .map(|_| *file_id)
            })
        })
    }

    fn get_file_paths_by_ids(&self, ids: &[i64]) -> Option<Vec<String>> {
        let mut paths = Vec::new();
        for id in ids {
            let path = self.file_paths_by_id.get(id)?;
            paths.push(path.clone());
        }
        Some(paths)
    }

    fn collect_all_file_ids(&self) -> Vec<i64> {
        self.all_file_ids.clone()
    }

    fn is_type_like_symbol(&self, symbol_name: &str) -> bool {
        self.class_like_names.contains(symbol_name)
    }

    fn find_type_reference_file_ids(&self, symbol_name: &str) -> Option<Vec<i64>> {
        self.find_symbol_call_file_ids(symbol_name)
    }

    fn get_member_declaration_results(&self, symbol_name: &str, owner_class: &str) -> Option<Vec<Value>> {
        let entries = self.member_decls_by_key.get(&member_key(symbol_name, owner_class))?;
        let mut results = Vec::new();
        for entry in entries {
            let context = context_line_for_path(&entry.path, entry.line);
            let col = find_identifier_in_line(&context, symbol_name).unwrap_or(0);
            let kind = classify_member_location(&entry.path, &context, symbol_name);
            push_unique_result(
                &mut results,
                json!({
                    "path": entry.path,
                    "line": entry.line,
                    "col": col,
                    "context": context,
                    "kind": kind,
                    "class_name": entry.class_name,
                }),
            );
        }
        Some(results)
    }
}

fn member_key(symbol_name: &str, owner_class: &str) -> String {
    format!("{}\t{}", owner_class, symbol_name)
}

fn dedupe_id_map(map: &mut HashMap<String, Vec<i64>>) {
    for ids in map.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
}

fn dedupe_call_site_map(map: &mut HashMap<String, Vec<CallSiteHotEntry>>) {
    for entries in map.values_mut() {
        entries.sort_by(|left, right| {
            left.file_id
                .cmp(&right.file_id)
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.path.cmp(&right.path))
        });
        entries.dedup_by(|left, right| {
            left.file_id == right.file_id && left.line == right.line && left.path == right.path
        });
    }
}

fn dedupe_i64_id_map(map: &mut HashMap<i64, Vec<i64>>) {
    for ids in map.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
}

fn find_type_reference_file_ids(conn: &Connection, symbol_name: &str) -> Result<Vec<i64>> {
    let paths = text::search_matching_paths(conn, symbol_name, MAX_FILES)?;
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let Some(file_id) = find_file_id(conn, &path)? else {
            continue;
        };
        if seen.insert(file_id) {
            ids.push(file_id);
        }
    }

    Ok(ids)
}

fn context_line_for_path(path: &str, line_number: i64) -> String {
    text::read_line_at(path, line_number.max(1) as usize).unwrap_or_default()
}

/// Find definition file ids for a member owned by a specific class.
/// 查找指定类成员定义所在的文件 id。
fn find_member_definition_file_ids(
    conn: &Connection,
    symbol_name: &str,
    owner_class: &str,
) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT file_id
        FROM search_symbols
        WHERE name = ?
          AND owner_name = ?
          AND is_class_like = 0
        "#,
        &[symbol_name, owner_class],
        &mut seen,
        &mut ids,
    )?;

    Ok(ids)
}

/// Find definition file ids for class/member/enum symbols matching one name.
/// 查找与符号名匹配的 class/member/enum 定义文件。
fn find_symbol_definition_file_ids(conn: &Connection, symbol_name: &str) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT file_id
        FROM search_symbols
        WHERE name = ?
        "#,
        &[symbol_name],
        &mut seen,
        &mut ids,
    )?;

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT ev.file_id
        FROM enum_values ev
        JOIN strings se ON ev.name_id = se.id
        WHERE se.text = ?
          AND ev.file_id IS NOT NULL
        "#,
        &[symbol_name],
        &mut seen,
        &mut ids,
    )?;

    Ok(ids)
}

/// Find files containing indexed symbol calls with the given name.
/// 查找包含指定调用名的文件。
fn find_symbol_call_file_ids(conn: &Connection, symbol_name: &str) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT file_id
        FROM search_symbol_calls
        WHERE name_lc = lower(?)
        "#,
        &[symbol_name],
        &mut seen,
        &mut ids,
    )?;

    Ok(ids)
}

fn get_direct_call_results(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    scope: Option<&UsageScope>,
    limit: usize,
) -> Result<Vec<Value>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let overscan_limit = limit.saturating_mul(8).clamp(limit, 4096);
    let entries = if let Some(entries) = hot_index.and_then(|index| index.get_direct_call_entries(symbol_name, overscan_limit)) {
        entries
    } else {
        let sql = r#"
            SELECT file_id, path, line_number
            FROM search_symbol_calls
            WHERE name_lc = lower(?)
            ORDER BY path_lc, line_number
            LIMIT ?
            "#;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![symbol_name, overscan_limit as i64], |row| {
            Ok(CallSiteHotEntry {
                file_id: row.get::<_, i64>(0)?,
                path: normalize_path(&row.get::<_, String>(1)?),
                line: row.get::<_, i64>(2)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        entries
    };

    let mut results = Vec::new();
    let mut content_cache = HashMap::<String, Option<String>>::new();
    let mut member_context_cache = HashMap::<String, Option<FileMemberContext>>::new();
    for entry in entries {
        if results.len() >= limit {
            break;
        }
        let item = text::TextLineMatch {
            path: entry.path.clone(),
            line_number: entry.line,
            line_text: context_line_for_path(&entry.path, entry.line),
        };
        let _ = process_indexed_line_match(
            symbol_name,
            &item,
            scope,
            &mut content_cache,
            &mut member_context_cache,
            &mut |value| {
                push_unique_result(&mut results, value);
                Ok(())
            },
        )?;
    }
    Ok(results)
}

/// Collect all indexed file ids as a last-resort unresolved usage scope fallback.
/// 作为最后兜底，收集所有已索引文件 id。
fn collect_all_file_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT file_id FROM search_files ORDER BY file_id")?;
    let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;

    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }

    Ok(ids)
}

/// Return declaration rows for a member owned by a specific class.
/// 返回指定类成员的声明行。
fn get_member_declaration_results(
    conn: &Connection,
    hot_index: Option<&UsageHotIndex>,
    symbol_name: &str,
    owner_class: &str,
) -> Result<Vec<Value>> {
    if let Some(results) = hot_index.and_then(|index| index.get_member_declaration_results(symbol_name, owner_class)) {
        return Ok(results);
    }

    let sql = r#"
        SELECT
            ss.path,
            ss.line_number,
            ss.owner_name
        FROM search_symbols ss
        WHERE ss.name = ?
          AND ss.owner_name = ?
          AND ss.is_class_like = 0
        ORDER BY ss.line_number
        "#;

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([symbol_name, owner_class], |row| {
        let path = normalize_path(&row.get::<_, String>(0)?);
        let line = row.get::<_, i64>(1)?;
        let class_name = row.get::<_, String>(2)?;
        let context = context_line_for_path(&path, line);
        let col = find_identifier_in_line(&context, symbol_name).unwrap_or(0);
        let kind = classify_member_location(&path, &context, symbol_name);

        Ok(json!({
            "path": path,
            "line": line,
            "col": col,
            "context": context,
            "kind": kind,
            "class_name": class_name,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        push_unique_result(&mut results, row?);
    }

    Ok(results)
}

/// Find files that include the definition files.
/// 查找 include 了定义文件的文件。
fn find_including_file_ids(conn: &Connection, def_ids: &[i64]) -> Result<Vec<i64>> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    if def_ids.is_empty() {
        return Ok(results);
    }

    for chunk in def_ids.chunks(SQL_CHUNK_SIZE) {
        let placeholders = repeat_placeholders(chunk.len());
        let sql = format!(
            r#"
            SELECT DISTINCT fi.file_id
            FROM file_includes fi
            WHERE fi.resolved_file_id IN ({})
            "#,
            placeholders
        );

        let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;

        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            if seen.insert(id) {
                results.push(id);
            }
        }
    }

    Ok(results)
}

/// Find one file id from full path or filename.
/// 通过完整路径或文件名查找 file id。
fn find_file_id(conn: &Connection, file_path: &str) -> Result<Option<i64>> {
    let normalized = normalize_path(file_path);

    if let Ok(id) = conn.query_row(
        "SELECT file_id FROM search_files WHERE path = ? LIMIT 1",
        [normalized.as_str()],
        |row| row.get::<_, i64>(0),
    ) {
        return Ok(Some(id));
    }

    let Some(filename) = std::path::Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return Ok(None);
    };

    let id = conn
        .query_row(
            r#"
            SELECT file_id
            FROM search_files
            WHERE basename = ?
            LIMIT 1
            "#,
            [filename],
            |row| row.get::<_, i64>(0),
        )
        .ok();

    Ok(id)
}

/// Collect ids from a simple one-parameter SQL query.
/// 从一个单参数 SQL 查询里收集 id。
fn collect_ids(
    conn: &Connection,
    sql: &str,
    params: &[&str],
    seen: &mut HashSet<i64>,
    ids: &mut Vec<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let sql_params = params
        .iter()
        .map(|param| &*param as &dyn ToSql)
        .collect::<Vec<_>>();
    let mut rows = stmt.query(rusqlite::params_from_iter(sql_params))?;

    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if seen.insert(id) {
            ids.push(id);
        }
    }

    Ok(())
}

/// Convert file ids to full file paths.
/// 把 file id 转换成完整文件路径。
fn get_file_paths_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<String>> {
    let mut results = Vec::new();

    if ids.is_empty() {
        return Ok(results);
    }

    for chunk in ids.chunks(SQL_CHUNK_SIZE) {
        let placeholders = repeat_placeholders(chunk.len());
        let sql = format!(
            r#"
            SELECT path
            FROM search_files
            WHERE file_id IN ({})
            ORDER BY path
            "#,
            placeholders
        );

        let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;

        while let Some(row) = rows.next()? {
            let path = row.get::<_, String>(0)?;
            results.push(normalize_path(&path));
        }
    }

    Ok(results)
}

// -----------------------------------------------------------------------------
// File text search
// -----------------------------------------------------------------------------

/// Search a single file line by line.
/// 逐行搜索单个文件。
fn search_in_file<F>(
    _conn: &Connection,
    path: &str,
    symbol_name: &str,
    remaining_limit: usize,
    scope: Option<&UsageScope>,
    mut on_match: F,
) -> Result<()>
where
    F: FnMut(Value) -> Result<()>,
{
    if remaining_limit == 0 {
        return Ok(());
    }

    let content = match scope {
        Some(UsageScope::Member(_)) => std::fs::read_to_string(path).ok(),
        _ => None,
    };
    let mut emitted = 0usize;
    let member_context = match scope {
        Some(UsageScope::Member(member_scope)) => {
            content
                .as_deref()
                .map(|content| FileMemberContext::new(content, member_scope))
        }
        _ => None,
    };

    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(()),
    };
    let reader = BufReader::new(file);
    for (index, line) in reader.lines().enumerate() {
        if emitted >= remaining_limit {
            break;
        }

        let current_line = index + 1;
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };

        if let Some(UsageScope::Local(local_scope)) = scope {
            if current_line < local_scope.start_line || current_line > local_scope.end_line {
                continue;
            }
        }

        let mut search_start = 0usize;

        while emitted < remaining_limit {
            let Some(col) = find_word_in_line_from(&line, symbol_name, search_start) else {
                break;
            };

            if !is_code_occurrence(&line, col) {
                search_start = col + symbol_name.len();
                continue;
            }

            if !should_emit_match(
                content.as_deref().unwrap_or(""),
                &line,
                current_line,
                col,
                scope,
                member_context.as_ref(),
            ) {
                search_start = col + symbol_name.len();
                continue;
            }

            on_match(json!({
                "path": normalize_path(path),
                "line": current_line,
                "col": col,
                "context": line.trim(),
                "kind": classify_usage_line(&line, symbol_name, col),
            }))?;

            emitted += 1;
            search_start = col + symbol_name.len();
        }
    }

    Ok(())
}

fn search_indexed_candidate_lines<F>(
    conn: &Connection,
    paths: &[String],
    symbol_name: &str,
    remaining_limit: usize,
    scope: Option<&UsageScope>,
    mut on_match: F,
) -> Result<bool>
where
    F: FnMut(Value) -> Result<()>,
{
    if remaining_limit == 0 || paths.is_empty() || symbol_name.trim().len() < 3 {
        return Ok(false);
    }

    let Some(primary_db_path) = text::current_primary_db_path(conn)? else {
        return Ok(false);
    };
    if !Path::new(&text::derived_text_db_path(&primary_db_path)).is_file() {
        return Ok(false);
    }

    let overscan_limit = remaining_limit.saturating_mul(24).clamp(96, 4096);
    let matches = text::search_matching_lines_in_paths(conn, symbol_name, paths, overscan_limit, 0)?;
    if matches.is_empty() {
        return Ok(true);
    }

    let mut emitted = 0usize;
    let mut content_cache = HashMap::<String, Option<String>>::new();
    let mut member_context_cache = HashMap::<String, Option<FileMemberContext>>::new();

    for item in matches {
        if emitted >= remaining_limit {
            break;
        }

        if process_indexed_line_match(
            symbol_name,
            &item,
            scope,
            &mut content_cache,
            &mut member_context_cache,
            &mut on_match,
        )? {
            emitted += 1;
        }
    }

    Ok(true)
}

fn process_indexed_line_match<F>(
    symbol_name: &str,
    item: &text::TextLineMatch,
    scope: Option<&UsageScope>,
    content_cache: &mut HashMap<String, Option<String>>,
    member_context_cache: &mut HashMap<String, Option<FileMemberContext>>,
    on_match: &mut F,
) -> Result<bool>
where
    F: FnMut(Value) -> Result<()>,
{
    let path = item.path.clone();
    let line = item.line_text.as_str();
    let current_line = item.line_number.max(1) as usize;

    if let Some(UsageScope::Local(local_scope)) = scope {
        if current_line < local_scope.start_line || current_line > local_scope.end_line {
            return Ok(false);
        }
    }

    let member_context = match scope {
        Some(UsageScope::Member(member_scope)) => {
            if !member_context_cache.contains_key(&path) {
                let content = content_cache
                    .entry(path.clone())
                    .or_insert_with(|| std::fs::read_to_string(&path).ok());
                let context = content
                    .as_deref()
                    .map(|content| FileMemberContext::new(content, member_scope));
                member_context_cache.insert(path.clone(), context);
            }
            member_context_cache.get(&path).and_then(|value| value.as_ref())
        }
        _ => None,
    };

    let content = content_cache
        .entry(path.clone())
        .or_insert_with(|| match scope {
            Some(UsageScope::Member(_)) => std::fs::read_to_string(&path).ok(),
            _ => None,
        })
        .as_deref()
        .unwrap_or("");

    let mut search_start = 0usize;
    let mut emitted = false;
    while let Some(col) = find_word_in_line_from(line, symbol_name, search_start) {
        if !is_code_occurrence(line, col) {
            search_start = col + symbol_name.len();
            continue;
        }

        if !should_emit_match(
            content,
            line,
            current_line,
            col,
            scope,
            member_context,
        ) {
            search_start = col + symbol_name.len();
            continue;
        }

        on_match(json!({
            "path": normalize_path(&path),
            "line": current_line,
            "col": col,
            "context": line.trim(),
            "kind": classify_usage_line(line, symbol_name, col),
        }))?;
        emitted = true;
        search_start = col + symbol_name.len();
    }

    Ok(emitted)
}

/// Classify a reference usage line into a human-friendly kind.
/// 将一条引用行分类成更友好的类型。
fn classify_usage_line(line: &str, symbol_name: &str, col: usize) -> &'static str {
    let col = col.min(line.len());
    let after_start = (col + symbol_name.len()).min(line.len());
    let before = &line[..col];
    let after = &line[after_start..];
    let left = before.trim_end();
    let right = after.trim_start();

    if line.contains("UPROPERTY(") || line.contains("UFUNCTION(") {
        return "declaration";
    }

    if left.contains("::") && right.starts_with('(') {
        return "definition";
    }

    if right.starts_with('(') {
        return "call";
    }

    if right.starts_with('=')
        || right.starts_with("+=")
        || right.starts_with("-=")
        || right.starts_with("*=")
        || right.starts_with("/=")
    {
        return "write";
    }

    "read"
}

/// Classify a member declaration row as declaration or definition.
/// 将成员声明结果分类为 declaration 或 definition。
fn classify_member_location(path: &str, context: &str, symbol_name: &str) -> &'static str {
    let path = path.to_ascii_lowercase();
    let is_cpp = path.ends_with(".cpp") || path.ends_with(".cc") || path.ends_with(".cxx");

    if is_cpp && context.contains("::") && context.contains(symbol_name) {
        return "definition";
    }

    "declaration"
}

struct FileMemberContext {
    methods: Vec<MethodRange>,
    candidate_classes: Vec<String>,
}

struct MethodRange {
    class_name: String,
    start_line: usize,
    end_line: usize,
}

impl FileMemberContext {
    fn new(content: &str, member_scope: &MemberScope) -> Self {
        let candidate_classes = member_scope
            .candidate_classes()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let methods = collect_method_ranges(content, &candidate_classes);

        Self {
            methods,
            candidate_classes,
        }
    }

    fn method_class_at(&self, line: usize) -> Option<&str> {
        self.methods
            .iter()
            .find(|range| line >= range.start_line && line <= range.end_line)
            .map(|range| range.class_name.as_str())
    }

    fn is_candidate_class(&self, class_name: &str) -> bool {
        self.candidate_classes
            .iter()
            .any(|candidate| candidate == class_name)
    }
}

fn collect_method_ranges(content: &str, candidate_classes: &[String]) -> Vec<MethodRange> {
    let mut ranges = Vec::new();
    let mut pending: Option<(String, usize)> = None;
    let mut active: Option<(String, usize, i32)> = None;

    for (index, line) in content.lines().enumerate() {
        let current_line = index + 1;

        if active.is_none() && pending.is_none() {
            if let Some(class_name) = detect_method_class(line, candidate_classes) {
                pending = Some((class_name.to_string(), current_line));
            }
        }

        if active.is_none() {
            if let Some((class_name, start_line)) = pending.take() {
                if line.contains('{') {
                    active = Some((class_name, start_line, 0));
                } else {
                    pending = Some((class_name, start_line));
                }
            }
        }

        if let Some((class_name, start_line, depth)) = active.as_mut() {
            *depth += count_char(line, '{') as i32;
            *depth -= count_char(line, '}') as i32;

            if *depth <= 0 && line.contains('}') {
                ranges.push(MethodRange {
                    class_name: class_name.clone(),
                    start_line: *start_line,
                    end_line: current_line,
                });
                active = None;
            }
        }
    }

    let total_lines = content.lines().count();
    if let Some((class_name, start_line, _)) = active {
        ranges.push(MethodRange {
            class_name,
            start_line,
            end_line: total_lines,
        });
    }

    ranges
}

/// Decide whether a text match is likely the target member reference.
/// 判断一次文本匹配是否像目标成员引用。
fn should_emit_match(
    content: &str,
    line: &str,
    line_number: usize,
    col: usize,
    scope: Option<&UsageScope>,
    member_context: Option<&FileMemberContext>,
) -> bool {
    let Some(scope) = scope else {
        return true;
    };

    let UsageScope::Member(member_scope) = scope else {
        return true;
    };

    let Some(member_context) = member_context else {
        return false;
    };

    if let Some((qualifier, op)) = explicit_qualifier_before(line, col) {
        if op == "::" {
            return member_context.is_candidate_class(&qualifier);
        }

        if qualifier == "this" {
            return member_context
                .method_class_at(line_number)
                .map(|class_name| member_context.is_candidate_class(class_name))
                .unwrap_or(false);
        }

        return goto::infer_var_type(content, &qualifier, Some(line_number.saturating_sub(1) as u32))
            .map(|ty| member_context.is_candidate_class(&ty))
            .unwrap_or(false);
    }

    if let Some(active_class) = member_context.method_class_at(line_number) {
        return member_context.is_candidate_class(active_class);
    }

    let _ = member_scope;
    false
}

fn is_code_occurrence(line: &str, col: usize) -> bool {
    if line
        .find("//")
        .map(|comment_start| col >= comment_start)
        .unwrap_or(false)
    {
        return false;
    }

    !is_inside_double_quoted_string(line, col)
}

fn is_inside_double_quoted_string(line: &str, col: usize) -> bool {
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if index >= col {
            break;
        }

        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == '"' {
            in_string = !in_string;
        }
    }

    in_string
}

fn explicit_qualifier_before(line: &str, col: usize) -> Option<(String, &'static str)> {
    let before = &line[..col];
    let trimmed = before.trim_end();

    let (prefix, op) = if let Some(prefix) = trimmed.strip_suffix("->") {
        (prefix, "->")
    } else if let Some(prefix) = trimmed.strip_suffix('.') {
        (prefix, ".")
    } else if let Some(prefix) = trimmed.strip_suffix("::") {
        (prefix, "::")
    } else {
        return None;
    };

    let qualifier = prefix
        .rsplit(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .next()
        .unwrap_or("")
        .trim();

    if qualifier.is_empty() {
        return None;
    }

    Some((qualifier.to_string(), op))
}

fn detect_method_class<'a>(line: &str, candidate_classes: &'a [String]) -> Option<&'a str> {
    if line.trim_start().starts_with("//") {
        return None;
    }

    candidate_classes
        .iter()
        .map(String::as_str)
        .find(|class_name| line.contains(&format!("{class_name}::")) && line.contains('('))
}

fn count_char(line: &str, target: char) -> usize {
    line.chars().filter(|ch| *ch == target).count()
}

fn push_unique_result(results: &mut Vec<Value>, item: Value) {
    let identity = usage_identity(&item);

    if results.iter().any(|existing| usage_identity(existing) == identity) {
        return;
    }

    results.push(item);
}

fn usage_identity(item: &Value) -> String {
    let path = item
        .get("path")
        .or_else(|| item.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = item
        .get("line")
        .or_else(|| item.get("line_number"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let col = item
        .get("col")
        .or_else(|| item.get("column"))
        .and_then(Value::as_i64)
        .unwrap_or_default();

    format!("{}:{}:{}", normalize_path(path), line, col)
}

fn find_identifier_in_line(line: &str, symbol_name: &str) -> Option<usize> {
    let mut start = 0usize;

    while start + symbol_name.len() <= line.len() {
        let Some(relative) = line[start..].find(symbol_name) else {
            return None;
        };

        let absolute = start + relative;
        let before_ok = absolute == 0
            || !is_word_char(line.as_bytes()[absolute.saturating_sub(1)]);
        let end = absolute + symbol_name.len();
        let after_ok = end >= line.len() || !is_word_char(line.as_bytes()[end]);

        if before_ok && after_ok {
            return Some(absolute);
        }

        start = absolute + 1;
    }

    None
}

/// Find a whole-word symbol occurrence in one line from an offset.
/// 从指定偏移开始，在一行里查找完整单词 symbol。
fn find_word_in_line_from(line: &str, symbol: &str, start_from: usize) -> Option<usize> {
    let symbol_len = symbol.len();

    if symbol_len == 0 || start_from >= line.len() {
        return None;
    }

    let bytes = line.as_bytes();
    let mut start = start_from;

    while start + symbol_len <= bytes.len() {
        let rel = line[start..].find(symbol)?;
        let abs = start + rel;

        if is_word_boundary(bytes, abs, symbol_len) {
            return Some(abs);
        }

        start = abs + 1;
    }

    None
}

/// Check whether the match has word boundaries on both sides.
/// 检查匹配结果两侧是否都是单词边界。
fn is_word_boundary(bytes: &[u8], start: usize, len: usize) -> bool {
    let end = start + len;

    let before_ok = start == 0 || !is_word_char(bytes[start - 1]);
    let after_ok = end >= bytes.len() || !is_word_char(bytes[end]);

    before_ok && after_ok
}

/// Return true for C/C++ identifier characters.
/// 判断是否是 C/C++ 标识符字符。
fn is_word_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_'
}

// -----------------------------------------------------------------------------
// Misc helpers
// -----------------------------------------------------------------------------

/// Create SQL placeholders like "?,?,?".
/// 生成 SQL 参数占位符，比如 "?,?,?"。
fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Normalize Windows paths to slash-separated paths.
/// 把 Windows 反斜杠路径统一成斜杠路径。
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").replace("//", "/")
}
