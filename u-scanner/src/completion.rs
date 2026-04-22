use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tree_sitter::{Node, Parser, Point};

use crate::server::state::CompletionCache;

const MAX_COMPLETION_ITEMS: usize = 2000;
const MAX_MEMBER_ITEMS_PER_CLASS: usize = 250;
const MAX_TYPEDEF_DEPTH: usize = 4;
const MIN_GLOBAL_PREFIX_LEN: usize = 2;

/// Per-request cache and lookup context.
/// 单次补全请求里的缓存和查询上下文。
struct CompletionContext<'a> {
    conn: &'a Connection,
    file_cache: HashMap<String, Vec<String>>,
    string_id_cache: HashMap<String, i64>,
    class_id_cache: HashMap<String, Vec<i64>>,
    inheritance_cache: HashMap<(String, String), bool>,
    current_file_id: Option<i64>,
    included_file_ids: Option<HashSet<i64>>,
}

impl<'a> CompletionContext<'a> {
    /// Create a new request context.
    /// 创建新的补全请求上下文。
    fn new(conn: &'a Connection, file_path: Option<&str>) -> Self {
        Self {
            conn,
            file_cache: HashMap::new(),
            string_id_cache: HashMap::new(),
            class_id_cache: HashMap::new(),
            inheritance_cache: HashMap::new(),
            current_file_id: file_path.and_then(|path| get_file_id_by_full_path(conn, path)),
            included_file_ids: None,
        }
    }

    /// Get string id from strings table.
    /// 从 strings 表获取字符串 id。
    fn string_id(&mut self, text: &str) -> Result<Option<i64>> {
        let text = text.trim();

        if text.is_empty() {
            return Ok(None);
        }

        if let Some(id) = self.string_id_cache.get(text) {
            return Ok(Some(*id));
        }

        let id = self
            .conn
            .query_row("SELECT id FROM strings WHERE text = ?", [text], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;

        if let Some(id) = id {
            self.string_id_cache.insert(text.to_string(), id);
        }

        Ok(id)
    }

    /// Find classes by name, supporting namespace fallback.
    /// 根据类名查 classes.id，支持 namespace 兜底。
    fn class_ids_by_name(&mut self, class_name: &str) -> Result<Vec<i64>> {
        let class_name = clean_type(class_name);

        if class_name.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(ids) = self.class_id_cache.get(&class_name) {
            return Ok(ids.clone());
        }

        let mut ids = Vec::new();

        if let Some(name_id) = self.string_id(&class_name)? {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM classes WHERE name_id = ? ORDER BY line_number",
            )?;

            ids = stmt
                .query_map([name_id], |row| row.get::<_, i64>(0))?
                .filter_map(|row| row.ok())
                .collect();
        }

        if ids.is_empty() && class_name.contains("::") {
            let short = class_name.rsplit("::").next().unwrap_or(&class_name);
            ids = self.class_ids_by_name(short)?;
        }

        ids = self.filter_class_ids_by_includes(ids);

        self.class_id_cache.insert(class_name, ids.clone());
        Ok(ids)
    }

    /// Lazily collect transitive include file ids.
    /// 懒加载当前文件递归 include 到的 file_id 集合。
    fn included_file_ids(&mut self) -> &HashSet<i64> {
        if self.included_file_ids.is_some() {
            return self.included_file_ids.as_ref().unwrap();
        }

        let mut included = HashSet::new();

        if let Some(root_id) = self.current_file_id {
            let mut queue = VecDeque::from([root_id]);

            while let Some(file_id) = queue.pop_front() {
                if !included.insert(file_id) {
                    continue;
                }

                if let Ok(mut stmt) = self.conn.prepare_cached(
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
        }

        self.included_file_ids = Some(included);
        self.included_file_ids.as_ref().unwrap()
    }

    /// Prefer class definitions reachable from current file includes.
    /// 同名类很多时，优先使用当前文件 include 链里能看到的定义。
    fn filter_class_ids_by_includes(&mut self, ids: Vec<i64>) -> Vec<i64> {
        if ids.len() <= 1 || self.current_file_id.is_none() {
            return ids;
        }

        let included = self.included_file_ids().clone();

        let filtered = ids
            .iter()
            .copied()
            .filter(|class_id| {
                self.conn
                    .query_row("SELECT file_id FROM classes WHERE id = ?", [class_id], |row| {
                        row.get::<_, Option<i64>>(0)
                    })
                    .ok()
                    .flatten()
                    .map(|file_id| included.contains(&file_id))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();

        if filtered.is_empty() {
            ids
        } else {
            filtered
        }
    }
}

// -----------------------------------------------------------------------------
// Public entry
// -----------------------------------------------------------------------------

/// Main completion entry point.
/// 补全主入口。
pub fn process_completion(
    conn: &Connection,
    content: &str,
    line: u32,
    character: u32,
    file_path: Option<String>,
    cache: Option<Arc<Mutex<CompletionCache>>>,
    persistent_cache: Option<Arc<Mutex<Connection>>>,
) -> Result<Value> {
    let mut ctx = CompletionContext::new(conn, file_path.as_deref());

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
    parser.set_language(&language)?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| anyhow::anyhow!("failed to parse current buffer"))?;

    let root = tree.root_node();
    let cursor_node = cursor_node(root, line, character)
        .ok_or_else(|| anyhow::anyhow!("no tree-sitter node at cursor"))?;

    if let Some(items) = complete_macro_specifiers(cursor_node, content) {
        return Ok(items);
    }

    if let Some(request) = member_completion_request(cursor_node, content) {
        let receiver_text = clean_type(node_text(request.receiver, content));
        let current_class = enclosing_class(cursor_node, content);

        if receiver_text == "Super" {
            if let Some(current_class) = current_class.as_deref() {
                let members = fetch_super_members(
                    &mut ctx,
                    current_class,
                    request.prefix,
                    cache,
                    persistent_cache,
                )?;

                return Ok(json!(members));
            }

            return Ok(json!([]));
        }

        let ty = resolve_expression_type(
            &mut ctx,
            request.receiver,
            root,
            content,
            line as usize,
        )?;

        if let Some(ty) = ty {
            let ty = resolve_typedef(&mut ctx, &ty)?;

            let members = fetch_members_recursive(
                &mut ctx,
                &ty,
                request.prefix,
                cache,
                persistent_cache,
                current_class.as_deref(),
            )?;

            return Ok(json!(members));
        }

        return Ok(json!([]));
    }

    let prefix = completion_prefix(cursor_node, content);
    let mut items = Vec::new();

    if !prefix.is_empty() {
        if let Some(current_class) = enclosing_class(cursor_node, content) {
            let members = fetch_members_recursive(
                &mut ctx,
                &current_class,
                Some(prefix.clone()),
                cache.clone(),
                persistent_cache.clone(),
                Some(&current_class),
            )?;

            items.extend(members);
        }
    }

    items.extend(ue_snippets(&prefix));

    if prefix.chars().count() >= MIN_GLOBAL_PREFIX_LEN {
        items.extend(fetch_global_symbols(conn, &prefix)?);
    }

    Ok(json!(dedupe_completion_items(items)))
}

/// Complete members for the direct parent of the current class.
/// 补全当前类直接父类的成员。
fn fetch_super_members(
    ctx: &mut CompletionContext,
    current_class: &str,
    prefix: Option<String>,
    memory_cache: Option<Arc<Mutex<CompletionCache>>>,
    persistent_cache: Option<Arc<Mutex<Connection>>>,
) -> Result<Vec<Value>> {
    let Some(parent_class) = direct_parent_class(ctx, current_class)? else {
        return Ok(Vec::new());
    };

    fetch_members_recursive(
        ctx,
        &parent_class,
        prefix,
        memory_cache,
        persistent_cache,
        Some(current_class),
    )
}

/// Return the first direct parent class name for a class.
/// 返回某个类的第一个直接父类名。
fn direct_parent_class(ctx: &mut CompletionContext, class_name: &str) -> Result<Option<String>> {
    for class_id in ctx.class_ids_by_name(class_name)? {
        for (_, parent_name) in parent_classes(ctx.conn, class_id)? {
            let parent_name = clean_type(&parent_name);

            if !parent_name.is_empty() {
                return Ok(Some(parent_name));
            }
        }
    }

    Ok(None)
}

// -----------------------------------------------------------------------------
// Cursor analysis
// -----------------------------------------------------------------------------

struct MemberCompletionRequest<'a> {
    receiver: Node<'a>,
    prefix: Option<String>,
}

/// Find node around cursor.
/// 获取光标附近的 tree-sitter node。
fn cursor_node(root: Node, line: u32, character: u32) -> Option<Node> {
    let row = line as usize;
    let col = character as usize;

    root.descendant_for_point_range(
        Point::new(row, col.saturating_sub(1)),
        Point::new(row, col),
    )
}

/// Detect member completion after ., ->, or ::.
/// 判断是否是 .、->、:: 后面的成员补全。
fn member_completion_request<'a>(
    node: Node<'a>,
    content: &str,
) -> Option<MemberCompletionRequest<'a>> {
    let mut current = Some(node);

    while let Some(n) = current {
        match n.kind() {
            "." | "->" | "::" => {
                let receiver = previous_meaningful_sibling(n)?;
                return Some(MemberCompletionRequest {
                    receiver,
                    prefix: None,
                });
            }

            "field_expression" => {
                let receiver = n
                    .child_by_field_name("argument")
                    .or_else(|| n.child(0))?;

                let prefix = n
                    .child_by_field_name("field")
                    .map(|field| node_text(field, content).trim().to_string())
                    .filter(|text| text != "." && text != "->" && !text.is_empty());

                return Some(MemberCompletionRequest { receiver, prefix });
            }

            "qualified_identifier" => {
                let receiver = n.child_by_field_name("scope")?;
                let prefix = n
                    .child_by_field_name("name")
                    .map(|name| node_text(name, content).trim().to_string())
                    .filter(|text| !text.is_empty());

                return Some(MemberCompletionRequest { receiver, prefix });
            }

            "ERROR" => {
                if let Some(request) = member_request_from_error(n, content) {
                    return Some(request);
                }
            }

            _ => {}
        }

        current = n.parent();
    }

    None
}

/// Recover member completion from an ERROR node.
/// 从 ERROR 节点里恢复成员补全上下文。
fn member_request_from_error<'a>(
    node: Node<'a>,
    _content: &str,
) -> Option<MemberCompletionRequest<'a>> {
    for index in (0..node.child_count()).rev() {
        let child = node.child(index as u32)?;
        if matches!(child.kind(), "." | "->" | "::") {
            let receiver = previous_meaningful_sibling(child)?;
            return Some(MemberCompletionRequest {
                receiver,
                prefix: None,
            });
        }
    }

    None
}

/// Get typed prefix under cursor.
/// 获取当前正在输入的补全前缀。
fn completion_prefix(node: Node, content: &str) -> String {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            node_text(node, content).trim().to_string()
        }
        _ => String::new(),
    }
}

/// Previous non-comment sibling.
/// 上一个有意义的 sibling。
fn previous_meaningful_sibling(node: Node) -> Option<Node> {
    let mut current = node.prev_sibling();

    while let Some(n) = current {
        if !matches!(n.kind(), "comment" | "\n" | "\r") {
            return Some(n);
        }

        current = n.prev_sibling();
    }

    None
}

// -----------------------------------------------------------------------------
// Type resolution
// -----------------------------------------------------------------------------

/// Resolve expression type for member completion.
/// 解析表达式类型，用于成员补全。
fn resolve_expression_type(
    ctx: &mut CompletionContext,
    node: Node,
    root: Node,
    content: &str,
    cursor_row: usize,
) -> Result<Option<String>> {
    match node.kind() {
        "this" => Ok(enclosing_class(node, content)),

        "identifier" | "field_identifier" | "type_identifier" | "namespace_identifier" => {
            let name = node_text(node, content).trim();

            if name == "this" {
                return Ok(enclosing_class(node, content));
            }

            if let Some(ty) = infer_variable_type(ctx, name, root, content, cursor_row)? {
                return Ok(Some(ty));
            }

            if let Some(class_name) = enclosing_class(node, content) {
                if let Some(return_type) = find_member_return_type(ctx, &class_name, name)? {
                    return Ok(Some(return_type));
                }
            }

            if is_known_type(ctx, name)? {
                return Ok(Some(name.to_string()));
            }

            Ok(None)
        }

        "qualified_identifier" => {
            let text = node_text(node, content).trim();

            if is_known_type(ctx, text)? {
                return Ok(Some(text.to_string()));
            }

            if let Some((class_name, member_name)) = text.rsplit_once("::") {
                return find_member_return_type(ctx, class_name, member_name);
            }

            Ok(None)
        }

        "call_expression" => {
            let Some(function) = node.child_by_field_name("function") else {
                return Ok(None);
            };

            if let Some(ty) = resolve_special_call_type(ctx, function, root, content, cursor_row)? {
                return Ok(Some(ty));
            }

            match function.kind() {
                "field_expression" => {
                    let object = function.child_by_field_name("argument").or_else(|| function.child(0));
                    let field = function.child_by_field_name("field");

                    if let (Some(object), Some(field)) = (object, field) {
                        if let Some(object_type) =
                            resolve_expression_type(ctx, object, root, content, cursor_row)?
                        {
                            return find_member_return_type(
                                ctx,
                                &object_type,
                                node_text(field, content).trim(),
                            );
                        }
                    }

                    Ok(None)
                }

                _ => {
                    let name = node_text(function, content).trim();

                    if let Some((class_name, method_name)) = name.rsplit_once("::") {
                        return find_member_return_type(ctx, class_name, method_name);
                    }

                    if let Some(class_name) = enclosing_class(node, content) {
                        return find_member_return_type(ctx, &class_name, name);
                    }

                    Ok(None)
                }
            }
        }

        "field_expression" => {
            let object = node.child_by_field_name("argument").or_else(|| node.child(0));
            let field = node.child_by_field_name("field");

            if let (Some(object), Some(field)) = (object, field) {
                if let Some(object_type) =
                    resolve_expression_type(ctx, object, root, content, cursor_row)?
                {
                    return find_member_return_type(ctx, &object_type, node_text(field, content));
                }
            }

            Ok(None)
        }

        "subscript_expression" => {
            let object = node.child_by_field_name("argument").or_else(|| node.child(0));

            if let Some(object) = object {
                if let Some(object_type) =
                    resolve_expression_type(ctx, object, root, content, cursor_row)?
                {
                    return Ok(Some(unwrap_container_type(&object_type)));
                }
            }

            Ok(None)
        }

        "parenthesized_expression" | "pointer_expression" | "reference_declarator" => {
            for child in node_children(node) {
                if !matches!(child.kind(), "(" | ")" | "*" | "&") {
                    return resolve_expression_type(ctx, child, root, content, cursor_row);
                }
            }

            Ok(None)
        }

        _ => Ok(None),
    }
}

/// Resolve known Unreal factory/cast calls.
/// 解析 Unreal 常见工厂函数或 Cast 调用的返回类型。
fn resolve_special_call_type(
    ctx: &mut CompletionContext,
    function: Node,
    root: Node,
    content: &str,
    cursor_row: usize,
) -> Result<Option<String>> {
    let text = node_text(function, content).trim();

    if let Some(template_type) = extract_template_call_type(text) {
        let function_name = text.split('<').next().unwrap_or("");

        if matches!(
            function_name,
            "Cast"
                | "CastChecked"
                | "ExactCast"
                | "NewObject"
                | "CreateWidget"
                | "CreateDefaultSubobject"
        ) {
            return Ok(Some(template_type));
        }
    }

    resolve_expression_type(ctx, function, root, content, cursor_row)
}

/// Infer local variable or parameter type.
/// 推断局部变量或参数类型。
fn infer_variable_type(
    ctx: &mut CompletionContext,
    target_name: &str,
    root: Node,
    content: &str,
    cursor_row: usize,
) -> Result<Option<String>> {
    let mut best: Option<(usize, String)> = None;

    scan_declarations(root, content, cursor_row, target_name, &mut best);

    if let Some((_, ty)) = best {
        if ty == "auto" {
            return infer_from_assignment_text(ctx, target_name, root, content, cursor_row);
        }

        return Ok(Some(clean_type(&ty)));
    }

    infer_from_assignment_text(ctx, target_name, root, content, cursor_row)
}

/// Recursively scan declarations before cursor.
/// 递归扫描光标前的声明。
fn scan_declarations(
    node: Node,
    content: &str,
    cursor_row: usize,
    target_name: &str,
    best: &mut Option<(usize, String)>,
) {
    if matches!(
        node.kind(),
        "declaration" | "field_declaration" | "parameter_declaration"
    ) {
        if let (Some(type_node), Some(decl_node)) = (
            node.child_by_field_name("type"),
            node.child_by_field_name("declarator"),
        ) {
            if declaration_contains_name(decl_node, content, target_name) {
                let row = node.start_position().row;

                if row <= cursor_row && best.as_ref().map(|(r, _)| row >= *r).unwrap_or(true) {
                    *best = Some((row, node_text(type_node, content).trim().to_string()));
                }
            }
        }
    }

    for child in node_children(node) {
        scan_declarations(child, content, cursor_row, target_name, best);
    }
}

/// Check if declarator contains target variable name.
/// 判断 declarator 是否包含目标变量名。
fn declaration_contains_name(node: Node, content: &str, target_name: &str) -> bool {
    if matches!(node.kind(), "identifier" | "field_identifier") {
        return node_text(node, content).trim() == target_name;
    }

    for child in node_children(node) {
        if declaration_contains_name(child, content, target_name) {
            return true;
        }
    }

    false
}

/// Infer type from assignment expression text.
/// 根据赋值表达式文本推断类型。
fn infer_from_assignment_text(
    _ctx: &mut CompletionContext,
    target_name: &str,
    root: Node,
    content: &str,
    cursor_row: usize,
) -> Result<Option<String>> {
    let mut result = None;
    scan_assignment_text(root, content, cursor_row, target_name, &mut result);
    Ok(result)
}

/// Scan assignment expressions for simple known patterns.
/// 扫描赋值表达式中的简单已知模式。
fn scan_assignment_text(
    node: Node,
    content: &str,
    cursor_row: usize,
    target_name: &str,
    result: &mut Option<String>,
) {
    if node.start_position().row > cursor_row || result.is_some() {
        return;
    }

    if node.kind() == "assignment_expression" {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");

        if let (Some(left), Some(right)) = (left, right) {
            if node_text(left, content).trim() == target_name {
                *result = infer_type_from_value_text(node_text(right, content));
                return;
            }
        }
    }

    for child in node_children(node) {
        scan_assignment_text(child, content, cursor_row, target_name, result);
    }
}

/// Infer type from common expression text.
/// 从常见表达式文本推断类型。
fn infer_type_from_value_text(text: &str) -> Option<String> {
    let text = text.trim();

    if let Some(ty) = extract_template_call_type(text) {
        return Some(ty);
    }

    let head = text.split('(').next()?.trim();

    if head.contains("::") {
        return Some(clean_type(head.rsplit_once("::")?.0));
    }

    Some(clean_type(head)).filter(|s| !s.is_empty())
}

// -----------------------------------------------------------------------------
// Member fetch
// -----------------------------------------------------------------------------

/// Fetch members recursively through inheritance.
/// 递归继承链获取成员补全。
fn fetch_members_recursive(
    ctx: &mut CompletionContext,
    class_name: &str,
    prefix: Option<String>,
    memory_cache: Option<Arc<Mutex<CompletionCache>>>,
    persistent_cache: Option<Arc<Mutex<Connection>>>,
    accessor_class: Option<&str>,
) -> Result<Vec<Value>> {
    let class_name = resolve_typedef(ctx, class_name)?;
    let prefix = prefix.unwrap_or_default();
    let accessor = accessor_class.unwrap_or("");
    let cache_key = format!("completion:{}:{}:{}", class_name, prefix, accessor);

    if let Some(items) = read_completion_cache(&cache_key, &memory_cache, &persistent_cache)? {
        return Ok(items);
    }

    let mut class_ids = ctx.class_ids_by_name(&class_name)?;

    if class_ids.is_empty() {
        if let Some(base) = class_name.split('<').next() {
            class_ids = ctx.class_ids_by_name(base)?;
        }
    }

    let mut queue = VecDeque::from(class_ids);
    let mut visited_ids = HashSet::new();
    let mut visited_names = HashSet::new();
    let mut seen_items = HashSet::new();
    let mut items = Vec::new();

    while let Some(class_id) = queue.pop_front() {
        if !visited_ids.insert(class_id) {
            continue;
        }

        let current_class_name = class_name_by_id(ctx.conn, class_id).unwrap_or_default();
        visited_names.insert(current_class_name.clone());

        append_members_for_class(
            ctx,
            class_id,
            &current_class_name,
            &prefix,
            accessor,
            &mut seen_items,
            &mut items,
        )?;

        append_enum_items(ctx.conn, class_id, &prefix, &mut seen_items, &mut items)?;

        for (parent_id, parent_name) in parent_classes(ctx.conn, class_id)? {
            if !parent_name.is_empty() && !visited_names.insert(parent_name.clone()) {
                continue;
            }

            if let Some(parent_id) = parent_id {
                queue.push_back(parent_id);
            }

            for id in ctx.class_ids_by_name(&parent_name)? {
                queue.push_back(id);
            }
        }

        if items.len() >= MAX_COMPLETION_ITEMS {
            break;
        }
    }

    write_completion_cache(&cache_key, &items, &memory_cache, &persistent_cache);

    Ok(items)
}

/// Append members from one class.
/// 添加某个 class 的成员补全。
fn append_members_for_class(
    ctx: &mut CompletionContext,
    class_id: i64,
    owner_class: &str,
    prefix: &str,
    accessor_class: &str,
    seen: &mut HashSet<String>,
    items: &mut Vec<Value>,
) -> Result<()> {
    let mut sql = String::from(
        r#"
        SELECT
            smn.text,
            smt.text,
            srt.text,
            m.access,
            m.detail,
            m.line_number,
            dp.full_path || '/' || sn.text
        FROM members m
        JOIN strings smn ON m.name_id = smn.id
        JOIN strings smt ON m.type_id = smt.id
        LEFT JOIN strings srt ON m.return_type_id = srt.id
        LEFT JOIN files f ON COALESCE(m.file_id, NULL) = f.id
        LEFT JOIN dir_paths dp ON f.directory_id = dp.id
        LEFT JOIN strings sn ON f.filename_id = sn.id
        WHERE m.class_id = ?
          AND (m.access IS NULL OR m.access != 'impl')
        "#,
    );

    if !prefix.is_empty() {
        sql.push_str(" AND smn.text LIKE ?");
    }

    sql.push_str(" ORDER BY smn.text LIMIT ");

    sql.push_str(&MAX_MEMBER_ITEMS_PER_CLASS.to_string());

    let mut stmt = ctx.conn.prepare(&sql)?;

    let mut rows = if prefix.is_empty() {
        stmt.query(params![class_id])?
    } else {
        stmt.query(params![class_id, format!("{}%", prefix)])?
    };

    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let member_type: String = row.get(1)?;
        let return_type: Option<String> = row.get(2)?;
        let access: Option<String> = row.get(3)?;
        let detail: Option<String> = row.get(4)?;
        let line: Option<usize> = row.get::<_, Option<i64>>(5)?.map(|v| v as usize);
        let file_path: Option<String> = row.get(6).ok().flatten();

        if !is_member_accessible(ctx, owner_class, accessor_class, access.as_deref())? {
            continue;
        }

        let detail_text = member_detail(return_type.as_deref(), owner_class);
        let dedupe_key = format!("{}:{}", name, detail_text);

        if !seen.insert(dedupe_key) {
            continue;
        }

        let documentation = file_path
            .as_deref()
            .and_then(|path| line.map(|line| extract_comment_from_file(path, line, &mut ctx.file_cache)))
            .unwrap_or_default();

        let documentation = merge_docs(documentation, detail);

        items.push(json!({
            "label": name,
            "kind": completion_kind(&member_type),
            "detail": detail_text,
            "documentation": documentation,
            "insertText": name,
            "sourceClass": owner_class,
        }));
    }

    Ok(())
}

/// Build a compact member detail string for completion menus.
/// 构造补全菜单里紧凑的成员说明文本。
fn member_detail(return_type: Option<&str>, owner_class: &str) -> String {
    let return_type = return_type
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .unwrap_or("member");

    if owner_class.is_empty() {
        return return_type.to_string();
    }

    format!("{} - {}", return_type, owner_class)
}

/// Check C++ access visibility.
/// 检查 C++ public/protected/private 可见性。
fn is_member_accessible(
    ctx: &mut CompletionContext,
    owner_class: &str,
    accessor_class: &str,
    access: Option<&str>,
) -> Result<bool> {
    let access = access.unwrap_or("");

    if accessor_class.is_empty() {
        return Ok(access.is_empty() || access == "public");
    }

    if accessor_class == owner_class {
        return Ok(true);
    }

    if access == "private" {
        return Ok(false);
    }

    if access == "protected" {
        return is_subclass_of(ctx, accessor_class, owner_class);
    }

    Ok(true)
}

/// Append enum values.
/// 添加 enum item 补全。
fn append_enum_items(
    conn: &Connection,
    class_id: i64,
    prefix: &str,
    seen: &mut HashSet<String>,
    items: &mut Vec<Value>,
) -> Result<()> {
    let mut sql = String::from(
        "SELECT sen.text FROM enum_values ev JOIN strings sen ON ev.name_id = sen.id WHERE ev.enum_id = ?",
    );

    if !prefix.is_empty() {
        sql.push_str(" AND sen.text LIKE ?");
    }

    sql.push_str(" ORDER BY sen.text");

    let mut stmt = conn.prepare(&sql)?;

    let mut rows = if prefix.is_empty() {
        stmt.query(params![class_id])?
    } else {
        stmt.query(params![class_id, format!("{}%", prefix)])?
    };

    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;

        if seen.insert(format!("enum:{}", name)) {
            items.push(json!({
                "label": name,
                "kind": 20,
                "detail": "enum item",
                "insertText": name,
            }));
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// DB helpers
// -----------------------------------------------------------------------------

/// Find member return type through inheritance.
/// 沿继承链查找成员返回类型。
fn find_member_return_type(
    ctx: &mut CompletionContext,
    class_name: &str,
    member_name: &str,
) -> Result<Option<String>> {
    let class_name = resolve_typedef(ctx, class_name)?;
    let mut queue = VecDeque::from(ctx.class_ids_by_name(&class_name)?);
    let mut visited = HashSet::new();

    while let Some(class_id) = queue.pop_front() {
        if !visited.insert(class_id) {
            continue;
        }

        let result = ctx
            .conn
            .query_row(
                r#"
                SELECT srt.text
                FROM members m
                JOIN strings sm ON m.name_id = sm.id
                LEFT JOIN strings srt ON m.return_type_id = srt.id
                WHERE m.class_id = ?
                  AND sm.text = ?
                ORDER BY
                    CASE WHEN srt.text IS NULL OR srt.text = 'void' THEN 1 ELSE 0 END,
                    length(srt.text) DESC
                LIMIT 1
                "#,
                params![class_id, member_name],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();

        if let Some(result) = result {
            return Ok(Some(clean_type(&result)));
        }

        for (parent_id, parent_name) in parent_classes(ctx.conn, class_id)? {
            if let Some(parent_id) = parent_id {
                queue.push_back(parent_id);
            }

            for id in ctx.class_ids_by_name(&parent_name)? {
                queue.push_back(id);
            }
        }
    }

    Ok(None)
}

/// Get parent classes from inheritance table.
/// 从 inheritance 表获取父类。
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

/// Get class name from class id.
/// 根据 class id 获取 class name。
fn class_name_by_id(conn: &Connection, class_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT s.text FROM classes c JOIN strings s ON c.name_id = s.id WHERE c.id = ?",
        [class_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Return true if child class derives from parent class.
/// 判断 child 是否继承自 parent。
fn is_subclass_of(ctx: &mut CompletionContext, child: &str, parent: &str) -> Result<bool> {
    if child == parent {
        return Ok(true);
    }

    let key = (child.to_string(), parent.to_string());

    if let Some(value) = ctx.inheritance_cache.get(&key) {
        return Ok(*value);
    }

    let parent_ids = ctx.class_ids_by_name(parent)?;
    let mut queue = VecDeque::from(ctx.class_ids_by_name(child)?);
    let mut visited = HashSet::new();

    while let Some(class_id) = queue.pop_front() {
        if parent_ids.contains(&class_id) {
            ctx.inheritance_cache.insert(key, true);
            return Ok(true);
        }

        if !visited.insert(class_id) {
            continue;
        }

        for (parent_id, parent_name) in parent_classes(ctx.conn, class_id)? {
            if parent_name == parent {
                ctx.inheritance_cache.insert(key, true);
                return Ok(true);
            }

            if let Some(parent_id) = parent_id {
                queue.push_back(parent_id);
            }

            for id in ctx.class_ids_by_name(&parent_name)? {
                queue.push_back(id);
            }
        }
    }

    ctx.inheritance_cache.insert(key, false);
    Ok(false)
}

/// Resolve simple typedef aliases.
/// 解析简单 typedef 别名。
fn resolve_typedef(ctx: &mut CompletionContext, type_name: &str) -> Result<String> {
    let mut current = clean_type(type_name);

    for _ in 0..MAX_TYPEDEF_DEPTH {
        if current.is_empty() || current == "void" || current == "T" {
            break;
        }

        let Some(name_id) = ctx.string_id(&current)? else {
            break;
        };

        let next = ctx
            .conn
            .query_row(
                r#"
                SELECT sb.text
                FROM classes c
                JOIN strings sb ON c.base_class_id = sb.id
                WHERE c.name_id = ?
                  AND c.symbol_type = 'typedef'
                LIMIT 1
                "#,
                [name_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();

        let Some(next) = next.map(|s| clean_type(&s)) else {
            break;
        };

        if next.is_empty() || next == current {
            break;
        }

        current = next;
    }

    Ok(current)
}

/// Check whether a type exists in DB.
/// 检查类型是否存在于 DB。
fn is_known_type(ctx: &mut CompletionContext, name: &str) -> Result<bool> {
    let name = clean_type(name);

    let Some(name_id) = ctx.string_id(&name)? else {
        return Ok(false);
    };

    let exists = ctx
        .conn
        .prepare("SELECT 1 FROM classes WHERE name_id = ? LIMIT 1")?
        .exists([name_id])?;

    Ok(exists)
}

// -----------------------------------------------------------------------------
// Global symbols, snippets, macro specifiers
// -----------------------------------------------------------------------------

/// Fetch global class/struct/enum completions.
/// 获取全局 class/struct/enum 补全。
fn fetch_global_symbols(conn: &Connection, prefix: &str) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT s.text, c.symbol_type
        FROM classes c
        JOIN strings s ON c.name_id = s.id
        WHERE s.text LIKE ?
          AND c.symbol_type IN ('class', 'struct', 'UCLASS', 'USTRUCT', 'enum', 'UENUM')
        ORDER BY s.text
        LIMIT 80
        "#,
    )?;

    let rows = stmt.query_map([format!("{}%", prefix)], |row| {
        let name: String = row.get(0)?;
        let symbol_type: String = row.get(1)?;

        Ok(json!({
            "label": name,
            "kind": match symbol_type.as_str() {
                "enum" | "UENUM" => 13,
                _ => 7,
            },
            "detail": symbol_type,
            "insertText": name,
        }))
    })?;

    Ok(rows.filter_map(|row| row.ok()).collect())
}

/// Complete Unreal macro specifiers.
/// 补全 Unreal 宏参数 specifier。
fn complete_macro_specifiers(node: Node, content: &str) -> Option<Value> {
    let mut current = Some(node);

    while let Some(n) = current {
        if matches!(n.kind(), "unreal_macro_argument_list" | "macro_argument_list") {
            let parent = n.parent()?;
            let text = node_text(parent, content);
            let macro_name = text.split('(').next()?.trim();
            return macro_specifiers(macro_name);
        }

        current = n.parent();
    }

    None
}

/// Return macro specifier items.
/// 返回宏参数补全项。
fn macro_specifiers(macro_name: &str) -> Option<Value> {
    let labels = match macro_name {
        "UPROPERTY" => vec![
            ("EditAnywhere", "property specifier"),
            ("EditDefaultsOnly", "property specifier"),
            ("BlueprintReadOnly", "property specifier"),
            ("BlueprintReadWrite", "property specifier"),
            ("Category", "property key"),
            ("meta", "metadata key"),
            ("VisibleAnywhere", "property specifier"),
            ("Transient", "property specifier"),
        ],

        "UFUNCTION" => vec![
            ("BlueprintCallable", "function specifier"),
            ("BlueprintPure", "function specifier"),
            ("BlueprintImplementableEvent", "function specifier"),
            ("BlueprintNativeEvent", "function specifier"),
            ("Category", "function key"),
            ("meta", "metadata key"),
        ],

        "UCLASS" | "USTRUCT" => vec![
            ("Blueprintable", "type specifier"),
            ("BlueprintType", "type specifier"),
            ("Abstract", "type specifier"),
            ("meta", "metadata key"),
        ],

        _ => return None,
    };

    Some(json!(
        labels
            .into_iter()
            .map(|(label, detail)| {
                json!({
                    "label": label,
                    "kind": 12,
                    "detail": detail,
                    "insertText": label,
                })
            })
            .collect::<Vec<_>>()
    ))
}

/// Unreal snippets and common helpers.
/// Unreal 常用 snippet 和 helper 补全。
fn ue_snippets(prefix: &str) -> Vec<Value> {
    let mut items = vec![
        snippet("UCLASS", "UCLASS($1)", "Unreal class macro"),
        snippet("USTRUCT", "USTRUCT($1)", "Unreal struct macro"),
        snippet("UENUM", "UENUM($1)", "Unreal enum macro"),
        snippet("UPROPERTY", "UPROPERTY($1)", "Unreal property macro"),
        snippet("UFUNCTION", "UFUNCTION($1)", "Unreal function macro"),
        snippet("GENERATED_BODY", "GENERATED_BODY()", "Unreal generated body"),
        snippet("Super::", "Super::", "Parent class scope"),
        snippet("GetWorld()", "GetWorld()", "Get current world"),
    ];

    if !prefix.is_empty() {
        let lower = prefix.to_ascii_lowercase();
        items.retain(|item| {
            item["label"]
                .as_str()
                .map(|label| label.to_ascii_lowercase().starts_with(&lower))
                .unwrap_or(false)
        });
    }

    items
}

/// Create one snippet item.
/// 创建一个 snippet 补全项。
fn snippet(label: &str, insert_text: &str, detail: &str) -> Value {
    json!({
        "label": label,
        "kind": 15,
        "detail": detail,
        "insertText": insert_text,
        "sortText": "00",
    })
}

// -----------------------------------------------------------------------------
// Cache helpers
// -----------------------------------------------------------------------------

/// Read completion cache.
/// 读取补全缓存。
fn read_completion_cache(
    key: &str,
    memory_cache: &Option<Arc<Mutex<CompletionCache>>>,
    persistent_cache: &Option<Arc<Mutex<Connection>>>,
) -> Result<Option<Vec<Value>>> {
    if let Some(cache) = memory_cache {
        if let Some(value) = cache.lock().get(key, "") {
            if let Some(items) = value.as_array() {
                return Ok(Some(items.clone()));
            }
        }
    }

    if let Some(cache) = persistent_cache {
        let conn = cache.lock();

        let blob = conn
            .query_row(
                "SELECT value FROM persistent_cache WHERE key = ?",
                [key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;

        if let Some(blob) = blob {
            let value: Value = serde_json::from_slice(&blob)?;

            if let Some(items) = value.as_array() {
                return Ok(Some(items.clone()));
            }
        }
    }

    Ok(None)
}

/// Write completion cache.
/// 写入补全缓存。
fn write_completion_cache(
    key: &str,
    items: &[Value],
    memory_cache: &Option<Arc<Mutex<CompletionCache>>>,
    persistent_cache: &Option<Arc<Mutex<Connection>>>,
) {
    let value = json!(items);

    if let Some(cache) = memory_cache {
        cache.lock().put(key, "", value.clone());
    }

    if let Some(cache) = persistent_cache {
        if let Ok(blob) = serde_json::to_vec(&value) {
            let now = unix_timestamp();

            let _ = cache.lock().execute(
                "INSERT OR REPLACE INTO persistent_cache (key, value, last_used) VALUES (?, ?, ?)",
                params![key, blob, now],
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Text helpers
// -----------------------------------------------------------------------------

/// Extract comment above a member declaration.
/// 提取成员声明上方的注释。
fn extract_comment_from_file(
    file_path: &str,
    line_number: usize,
    file_cache: &mut HashMap<String, Vec<String>>,
) -> String {
    if line_number == 0 {
        return String::new();
    }

    if !file_cache.contains_key(file_path) {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            return String::new();
        };

        file_cache.insert(
            file_path.to_string(),
            content.lines().map(|line| line.to_string()).collect(),
        );
    }

    let Some(lines) = file_cache.get(file_path) else {
        return String::new();
    };

    let mut index = line_number.saturating_sub(1);
    let mut comments = Vec::new();
    let mut block_mode = false;

    while index > 0 {
        let text = lines[index - 1].trim();

        if text.is_empty()
            || text.starts_with("UPROPERTY")
            || text.starts_with("UFUNCTION")
            || text.starts_with("GENERATED_BODY")
        {
            index -= 1;
            continue;
        }

        if text.starts_with("//") {
            comments.push(text.trim_start_matches('/').trim().to_string());
            index -= 1;
            continue;
        }

        if text.ends_with("*/") {
            block_mode = true;
            comments.push(
                text.trim_end_matches("*/")
                    .trim_start_matches('*')
                    .trim()
                    .to_string(),
            );
            index -= 1;
            continue;
        }

        if block_mode {
            comments.push(
                text.trim_start_matches("/*")
                    .trim_start_matches('*')
                    .trim()
                    .to_string(),
            );

            if text.starts_with("/*") {
                break;
            }

            index -= 1;
            continue;
        }

        break;
    }

    comments.reverse();
    comments.into_iter().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
}

/// Merge comment documentation and DB detail.
/// 合并注释文档和 DB detail。
fn merge_docs(comment: String, detail: Option<String>) -> String {
    match (comment.is_empty(), detail) {
        (true, Some(detail)) => detail,
        (false, Some(detail)) if !detail.is_empty() => format!("{}\n\n{}", comment, detail),
        _ => comment,
    }
}

/// Clean C++/Unreal type text.
/// 清理 C++/Unreal 类型文本。
fn clean_type(raw: &str) -> String {
    let mut text = raw.trim().to_string();

    for keyword in [
        "const",
        "typename",
        "struct",
        "class",
        "enum",
        "virtual",
        "static",
        "inline",
        "FORCEINLINE",
        "volatile",
        "mutable",
    ] {
        text = text.replace(keyword, " ");
    }

    text = text.replace('*', " ").replace('&', " ");

    if let Some(inner) = extract_wrapped_type(&text) {
        return clean_type(&inner);
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract inner type from Unreal wrapper types.
/// 从 Unreal 包装类型中提取内部类型。
fn extract_wrapped_type(text: &str) -> Option<String> {
    let start = text.find('<')?;
    let end = text.rfind('>')?;
    let wrapper = text[..start].trim();

    if matches!(
        wrapper,
        "TObjectPtr"
            | "TWeakObjectPtr"
            | "TSoftObjectPtr"
            | "TSoftClassPtr"
            | "TSubclassOf"
            | "TSharedPtr"
            | "TSharedRef"
            | "TUniquePtr"
            | "TEnumAsByte"
    ) {
        return Some(text[start + 1..end].trim().to_string());
    }

    None
}

/// Extract type argument from template call text.
/// 从模板调用文本里提取类型参数。
fn extract_template_call_type(text: &str) -> Option<String> {
    let start = text.find('<')?;
    let end = text.rfind('>')?;

    Some(clean_type(&text[start + 1..end]))
}

/// Unwrap container element type.
/// 拆出容器元素类型。
fn unwrap_container_type(ty: &str) -> String {
    let Some(start) = ty.find('<') else {
        return clean_type(ty);
    };

    let Some(end) = ty.rfind('>') else {
        return clean_type(ty);
    };

    let wrapper = ty[..start].trim();
    let inner = &ty[start + 1..end];

    match wrapper {
        "TArray" | "TSet" => clean_type(inner),
        "TMap" => clean_type(template_argument(inner, 1)),
        _ => clean_type(ty),
    }
}

/// Get template argument by index.
/// 获取第 index 个模板参数。
fn template_argument(inner: &str, index: usize) -> &str {
    let mut depth = 0usize;
    let mut arg_index = 0usize;
    let mut start = 0usize;

    for (i, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if arg_index == index {
                    return inner[start..i].trim();
                }

                arg_index += 1;
                start = i + 1;
            }
            _ => {}
        }
    }

    if arg_index == index {
        inner[start..].trim()
    } else {
        ""
    }
}

/// Get enclosing class name.
/// 获取当前 node 所在 class 名。
fn enclosing_class(node: Node, content: &str) -> Option<String> {
    let mut current = Some(node);

    while let Some(node) = current {
        match node.kind() {
            "class_specifier"
            | "struct_specifier"
            | "unreal_class_declaration"
            | "unreal_struct_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    return Some(clean_type(node_text(name, content)));
                }
            }

            "function_definition" => {
                if let Some(decl) = node.child_by_field_name("declarator") {
                    if let Some(scope) = find_qualified_scope(decl) {
                        return Some(clean_type(node_text(scope, content)));
                    }
                }
            }

            _ => {}
        }

        current = node.parent();
    }

    None
}

/// Find scope inside qualified_identifier.
/// 查找 qualified_identifier 里的 scope。
fn find_qualified_scope(node: Node) -> Option<Node> {
    if node.kind() == "qualified_identifier" {
        return node.child_by_field_name("scope");
    }

    for child in node_children(node) {
        if let Some(found) = find_qualified_scope(child) {
            return Some(found);
        }
    }

    None
}

/// Get node text.
/// 获取 node 文本。
fn node_text<'a>(node: Node, content: &'a str) -> &'a str {
    node.utf8_text(content.as_bytes()).unwrap_or("")
}

/// Collect node children.
/// 收集 node 子节点。
fn node_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Completion item kind mapping.
/// 补全 item kind 映射。
fn completion_kind(kind: &str) -> i64 {
    match kind {
        "function" => 2,
        "property" | "variable" | "field" => 5,
        "enum_item" => 20,
        _ => 1,
    }
}

/// Remove duplicate completion labels.
/// 去重补全项。
fn dedupe_completion_items(items: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for item in items {
        let Some(label) = item.get("label").and_then(|label| label.as_str()) else {
            continue;
        };

        if seen.insert(label.to_string()) {
            result.push(item);
        }
    }

    result
}

/// Get current Unix timestamp.
/// 获取当前 Unix 时间戳。
fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve file_id by full path.
/// 根据完整路径解析 DB file_id。
fn get_file_id_by_full_path(conn: &Connection, file_path: &str) -> Option<i64> {
    let normalized = file_path.replace('\\', "/");
    let path = std::path::Path::new(&normalized);
    let filename = path.file_name()?.to_str()?;

    let sql = r#"
        SELECT f.id
        FROM files f
        JOIN directories d ON f.directory_id = d.id
        JOIN strings sf ON f.filename_id = sf.id
        WHERE sf.text = ?
        LIMIT 1
    "#;

    conn.query_row(sql, [filename], |row| row.get::<_, i64>(0))
        .optional()
        .ok()
        .flatten()
}
