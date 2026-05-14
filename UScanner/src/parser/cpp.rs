use std::cell::RefCell;
use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Context;
use memmap2::Mmap;
use regex::Regex;
use sha2::{Digest, Sha256};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

use crate::types::{ClassInfo, InputFile, MemberInfo, ParseData, ParseResult};

/// Regexes reused while cleaning C++ / Unreal type prefixes.
/// 清理 C++ / Unreal 类型前缀时复用的正则，避免每次解析都重新编译。
struct CleanRegexes {
    keywords: Vec<Regex>,
    api: Regex,
    unreal_macros: Regex,
    whitespace: Regex,
}

/// Lazily initialized regex cache.
/// 懒加载的正则缓存。
static CLEAN_REGEXES: OnceLock<CleanRegexes> = OnceLock::new();
static GAMEPLAY_TAG_DEFINE_RE: OnceLock<Regex> = OnceLock::new();
static GAMEPLAY_TAG_DECLARE_RE: OnceLock<Regex> = OnceLock::new();
static MACRO_DEFINE_RE: OnceLock<Regex> = OnceLock::new();
static DELEGATE_MACRO_START_RE: OnceLock<Regex> = OnceLock::new();

fn get_clean_regexes() -> &'static CleanRegexes {
    CLEAN_REGEXES.get_or_init(|| {
        let keywords = [
            "virtual",
            "static",
            "inline",
            "FORCEINLINE",
            "FORCEINLINE_DEBUGGABLE",
            "constexpr",
            "const",
            "friend",
            "class",
            "struct",
            "enum",
            "typename",
        ];

        CleanRegexes {
            keywords: keywords
                .iter()
                .map(|kw| Regex::new(&format!(r"\b{}\b", regex::escape(kw))).unwrap())
                .collect(),

            // Matches module export macros such as MYGAME_API.
            // 匹配 Unreal 模块导出宏，例如 MYGAME_API。
            api: Regex::new(r"\b[A-Z0-9_]+_API\b").unwrap(),

            // Matches common Unreal reflection macros before return/property types.
            // 匹配返回类型或属性类型前面的 Unreal 反射宏。
            unreal_macros: Regex::new(
                r"\bU(?:CLASS|STRUCT|ENUM|FUNCTION|PROPERTY|INTERFACE|DELEGATE|META)\s*\([^)]*\)",
            )
            .unwrap(),

            whitespace: Regex::new(r"\s+").unwrap(),
        }
    })
}

fn gameplay_tag_define_re() -> &'static Regex {
    GAMEPLAY_TAG_DEFINE_RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            \b(?:UE_DEFINE_GAMEPLAY_TAG(?:_COMMENT|_STATIC)?|DEFINE_GAMEPLAY_TAG(?:_COMMENT)?)\s*
            \(\s*
            (?P<identifier>[A-Za-z_][A-Za-z0-9_]*)\s*,\s*
            (?:TEXT\s*\(\s*)?
            " (?P<tag>[^"]+) "
            "#,
        )
        .unwrap()
    })
}

fn gameplay_tag_declare_re() -> &'static Regex {
    GAMEPLAY_TAG_DECLARE_RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            \bUE_DECLARE_GAMEPLAY_TAG_EXTERN\s*
            \(\s*
            (?P<identifier>[A-Za-z_][A-Za-z0-9_]*)
            \s*\)
            "#,
        )
        .unwrap()
    })
}

fn macro_define_re() -> &'static Regex {
    MACRO_DEFINE_RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^[ \t]*#[ \t]*define[ \t]+(?P<identifier>[A-Za-z_][A-Za-z0-9_]*)\b"#,
        )
        .unwrap()
    })
}

fn delegate_macro_start_re() -> &'static Regex {
    DELEGATE_MACRO_START_RE.get_or_init(|| {
        Regex::new(r"\b(?P<macro>DECLARE_[A-Za-z0-9_]+)\s*\(").unwrap()
    })
}

thread_local! {
    /// Per-thread parser reused across files.
    /// 每个线程复用一个 parser，减少重复分配。
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());

    /// Per-thread query cursor reused across files.
    /// 每个线程复用一个 query cursor。
    static CURSOR: RefCell<QueryCursor> = RefCell::new(QueryCursor::new());
}

/// Main symbol query for Unreal C++.
/// Unreal C++ 主符号查询。
///
/// This query extracts:
/// - class / struct / enum definitions
/// - Unreal reflected declarations
/// - base classes
/// - functions and fields
/// - enum items
/// - function calls and member calls
///
/// 这个 query 用来提取：
/// - class / struct / enum 定义
/// - Unreal 反射声明
/// - 父类
/// - 函数和字段
/// - 枚举项
/// - 函数调用和成员调用
pub const QUERY_STR: &str = r#"
  ; ========================
  ; Type definitions
  ; 类型定义
  ; ========================

  (class_specifier
    name: (type_identifier) @class_name) @class_def

  (struct_specifier
    name: (type_identifier) @struct_name) @struct_def

  (enum_specifier
    name: (type_identifier) @enum_name) @enum_def

  ; UTreeSitter reflected declarations.
  ; UTreeSitter 的 Unreal 反射声明节点。

  (unreal_reflected_class_declaration
    name: [
      (type_identifier) @class_name
      (qualified_identifier) @class_name
    ]) @uclass_def

  (unreal_reflected_struct_declaration
    name: [
      (type_identifier) @struct_name
      (qualified_identifier) @struct_name
    ]) @ustruct_def

  (unreal_reflected_enum_declaration
    name: [
      (type_identifier) @enum_name
      (qualified_identifier) @enum_name
    ]) @uenum_def

  ; Base classes.
  ; 父类列表。
  (base_class_clause
    (access_specifier)?
    [
      (type_identifier) @base_class_name
      (qualified_identifier) @base_class_name
    ])

  ; ========================
  ; Members and functions
  ; 成员和函数
  ; ========================

  (function_definition) @func_node
  (declaration) @decl_node
  (field_declaration) @field_node
  (unreal_function_declaration) @ufunc_node

  (enumerator
    name: (identifier) @enum_val_name) @enum_item

  ; ========================
  ; Calls
  ; 调用
  ; ========================

  (call_expression
    function: [
      (identifier) @call_name
      (qualified_identifier
        name: (identifier) @call_name)
      (field_expression
        field: (field_identifier) @call_name)
      (field_expression
        field: (template_method
          name: (field_identifier) @call_name))
      (template_function
        name: (identifier) @call_name)
    ]) @call_expr

  (field_expression
    field: (field_identifier) @field_name) @field_expr
"#;

/// Include query kept separate because includes are used for dependency graphing.
/// include query 单独保留，因为 include 通常用于依赖图分析。
pub const INCLUDE_QUERY_STR: &str = r#"
  (preproc_include
    path: [
      (string_literal) @path
      (system_lib_string) @path
    ]) @include
"#;

/// Parse one file and return structured symbol data.
/// 解析单个文件并返回结构化符号数据。
pub fn process_file(
    input: &InputFile,
    language: &tree_sitter::Language,
    query: &Query,
    include_query: &Query,
) -> anyhow::Result<ParseResult> {
    let file = File::open(&input.path)
        .with_context(|| format!("failed to open {}", input.path))?;

    // Memory-map the file to avoid copying large source files into memory.
    // 使用 mmap 读取文件，避免把大文件重复复制到内存。
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("failed to mmap {}", input.path))?;

    let content_bytes = &mmap[..];

    // Content hash is used to skip unchanged files.
    // 通过内容 hash 跳过未变化的文件。
    let mut hasher = Sha256::new();
    hasher.update(content_bytes);
    let new_hash = hex::encode(hasher.finalize());

    if input.old_hash.as_ref() == Some(&new_hash) {
        return Ok(ParseResult {
            path: input.path.clone(),
            status: "cache_hit".to_string(),
            mtime: input.mtime,
            data: None,
            module_id: input.module_id,
        });
    }

    let ext = Path::new(&input.path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let is_header = matches!(ext.as_str(), "h" | "hpp" | "hh" | "inl");

    // Header files are numerous in Unreal projects, so quickly skip headers
    // that do not contain useful Unreal/C++ indexing markers.
    // Unreal 项目头文件很多，所以先快速跳过没有关键标记的头文件。
    if is_header && !looks_like_interesting_unreal_header(content_bytes) {
        return Ok(ParseResult {
            path: input.path.clone(),
            status: "parsed".to_string(),
            mtime: input.mtime,
            data: Some(ParseData {
                classes: Vec::new(),
                calls: Vec::new(),
                includes: Vec::new(),
                gameplay_tags: Vec::new(),
                macro_definitions: Vec::new(),
                parser: "fast-skip".to_string(),
                new_hash,
            }),
            module_id: input.module_id,
        });
    }

    let (mut classes, calls, includes) =
        parse_content_mmap(content_bytes, &input.path, language, query, include_query)?;
    classes.extend(collect_delegate_definitions(content_bytes));
    let gameplay_tags = collect_gameplay_tags(content_bytes);
    let macro_definitions = collect_macro_definitions(content_bytes);

    Ok(ParseResult {
        path: input.path.clone(),
        status: "parsed".to_string(),
        mtime: input.mtime,
        data: Some(ParseData {
            classes,
            calls,
            includes,
            gameplay_tags,
            macro_definitions,
            parser: "treesitter".to_string(),
            new_hash,
        }),
        module_id: input.module_id,
    })
}

/// Parse already-loaded bytes.
/// 解析已经加载到内存中的源码字节。
pub fn parse_content_mmap(
    content_bytes: &[u8],
    _path: &str,
    language: &tree_sitter::Language,
    query: &Query,
    include_query: &Query,
) -> anyhow::Result<(
    Vec<ClassInfo>,
    Vec<crate::types::CallInfo>,
    Vec<String>,
)> {
    PARSER.with(|p_cell| {
        let mut parser = p_cell.borrow_mut();
        parser
            .set_language(language)
            .context("failed to set tree-sitter language")?;

        let tree = parser
            .parse(content_bytes, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
        let root = tree.root_node();

        CURSOR.with(|c_cell| {
            let mut cursor = c_cell.borrow_mut();

            let mut classes: Vec<ClassInfo> = Vec::new();
            let mut calls: Vec<crate::types::CallInfo> = Vec::new();
            let mut includes: Vec<String> = Vec::new();
            let mut pending_members: Vec<(MemberInfo, usize, usize)> = Vec::new();

            collect_includes(&mut cursor, include_query, root, content_bytes, &mut includes);
            collect_symbols(
                &mut cursor,
                query,
                root,
                content_bytes,
                &mut classes,
                &mut calls,
                &mut pending_members,
            );
            merge_reflected_type_fallbacks(root, content_bytes, &mut classes);

            attach_pending_members(&mut classes, pending_members);
            normalize_member_access(content_bytes, &mut classes);

            Ok((classes, calls, includes))
        })
    })
}

/// Convenience parser for string content.
/// 用于测试或内存字符串的便捷解析入口。
pub fn parse_content(
    content: &str,
    path: &str,
    language: &tree_sitter::Language,
    query: &Query,
) -> anyhow::Result<(
    Vec<ClassInfo>,
    Vec<crate::types::CallInfo>,
    Vec<String>,
)> {
    let include_query = Query::new(language, INCLUDE_QUERY_STR)
        .context("failed to compile include query")?;

    parse_content_mmap(content.as_bytes(), path, language, query, &include_query)
}

/// Cheap header pre-filter.
/// 低成本头文件预过滤。
fn looks_like_interesting_unreal_header(content: &[u8]) -> bool {
    contains_bytes(content, b"#include")
        || contains_bytes(content, b"#define")
        || contains_bytes(content, b"UCLASS")
        || contains_bytes(content, b"USTRUCT")
        || contains_bytes(content, b"UENUM")
        || contains_bytes(content, b"UINTERFACE")
        || contains_bytes(content, b"UDELEGATE")
        || contains_bytes(content, b"UFUNCTION")
        || contains_bytes(content, b"UPROPERTY")
        || contains_bytes(content, b"GENERATED_BODY")
        || contains_bytes(content, b"DECLARE_")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Collect GameplayTag declarations and definitions with lightweight regexes.
/// 使用轻量正则收集 GameplayTag 的声明和定义。
fn collect_gameplay_tags(content_bytes: &[u8]) -> Vec<crate::types::GameplayTagInfo> {
    let Ok(content) = std::str::from_utf8(content_bytes) else {
        return Vec::new();
    };

    let mut items = Vec::new();

    for caps in gameplay_tag_define_re().captures_iter(content) {
        let (Some(full), Some(identifier), Some(tag)) =
            (caps.get(0), caps.name("identifier"), caps.name("tag"))
        else {
            continue;
        };

        items.push(crate::types::GameplayTagInfo {
            identifier: identifier.as_str().to_string(),
            tag_path: Some(tag.as_str().to_string()),
            kind: "define".to_string(),
            line: line_number_for_offset(content, full.start()),
        });
    }

    for caps in gameplay_tag_declare_re().captures_iter(content) {
        let (Some(full), Some(identifier)) = (caps.get(0), caps.name("identifier")) else {
            continue;
        };

        if items
            .iter()
            .any(|item| item.identifier == identifier.as_str() && item.kind == "define")
        {
            continue;
        }

        items.push(crate::types::GameplayTagInfo {
            identifier: identifier.as_str().to_string(),
            tag_path: None,
            kind: "declare".to_string(),
            line: line_number_for_offset(content, full.start()),
        });
    }

    items
}

/// Collect generic C/C++ macro definitions.
/// 收集通用 C/C++ 宏定义。
fn collect_macro_definitions(content_bytes: &[u8]) -> Vec<crate::types::MacroDefinitionInfo> {
    let Ok(content) = std::str::from_utf8(content_bytes) else {
        return Vec::new();
    };

    let mut items = Vec::new();

    for caps in macro_define_re().captures_iter(content) {
        let (Some(full), Some(identifier)) = (caps.get(0), caps.name("identifier")) else {
            continue;
        };

        items.push(crate::types::MacroDefinitionInfo {
            name: identifier.as_str().to_string(),
            line: line_number_for_offset(content, full.start()),
        });
    }

    items
}

/// Collect Unreal delegate declarations produced by DECLARE_* macros.
/// 收集由 DECLARE_* 宏生成的 Unreal delegate 声明。
fn collect_delegate_definitions(content_bytes: &[u8]) -> Vec<ClassInfo> {
    let Ok(content) = std::str::from_utf8(content_bytes) else {
        return Vec::new();
    };

    let mut items = Vec::new();

    for caps in delegate_macro_start_re().captures_iter(content) {
        let Some(macro_match) = caps.name("macro") else {
            continue;
        };

        let macro_name = macro_match.as_str();
        if !looks_like_delegate_macro(macro_name) {
            continue;
        }

        let Some(open_paren) = content[macro_match.end()..]
            .find('(')
            .map(|offset| macro_match.end() + offset)
        else {
            continue;
        };

        let Some(close_paren) = find_matching_paren(content, open_paren) else {
            continue;
        };

        let args = &content[open_paren + 1..close_paren];
        let Some(delegate_name) = extract_delegate_name(macro_name, args) else {
            continue;
        };

        let line = line_number_for_offset(content, macro_match.start());
        let end_line = line_number_for_offset(content, close_paren);

        items.push(ClassInfo {
            class_name: delegate_name,
            namespace: None,
            base_classes: Vec::new(),
            symbol_type: delegate_symbol_type(macro_name).to_string(),
            line,
            end_line,
            range_start: macro_match.start(),
            range_end: close_paren + 1,
            members: Vec::new(),
            is_final: false,
            is_interface: false,
        });
    }

    dedupe_delegate_classes(items)
}

fn looks_like_delegate_macro(macro_name: &str) -> bool {
    let macro_upper = macro_name.to_ascii_uppercase();
    macro_upper.contains("DELEGATE") || macro_upper.starts_with("DECLARE_EVENT")
}

fn delegate_symbol_type(macro_name: &str) -> &'static str {
    let macro_upper = macro_name.to_ascii_uppercase();

    if macro_upper.starts_with("DECLARE_EVENT") {
        "event"
    } else if macro_upper.contains("DYNAMIC") && macro_upper.contains("MULTICAST") {
        "dynamic_multicast_delegate"
    } else if macro_upper.contains("DYNAMIC") {
        "dynamic_delegate"
    } else if macro_upper.contains("MULTICAST") {
        "multicast_delegate"
    } else {
        "delegate"
    }
}

fn find_matching_paren(content: &str, open_paren: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    let mut index = open_paren;

    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }

        index += 1;
    }

    None
}

fn split_top_level_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut brace_depth = 0i32;

    for ch in text.chars() {
        match ch {
            '(' => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' => {
                paren_depth -= 1;
                current.push(ch);
            }
            '<' => {
                angle_depth += 1;
                current.push(ch);
            }
            '>' => {
                angle_depth -= 1;
                current.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                current.push(ch);
            }
            ']' => {
                bracket_depth -= 1;
                current.push(ch);
            }
            '{' => {
                brace_depth += 1;
                current.push(ch);
            }
            '}' => {
                brace_depth -= 1;
                current.push(ch);
            }
            ',' if paren_depth == 0 && angle_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                let arg = current.trim();
                if !arg.is_empty() {
                    args.push(arg.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let arg = current.trim();
    if !arg.is_empty() {
        args.push(arg.to_string());
    }

    args
}

fn extract_delegate_name(macro_name: &str, args: &str) -> Option<String> {
    let args = split_top_level_args(args);
    if args.is_empty() {
        return None;
    }

    let macro_upper = macro_name.to_ascii_uppercase();
    let name_index = if macro_upper.starts_with("DECLARE_EVENT") || macro_upper.contains("RETVAL") {
        1
    } else {
        0
    };
    let raw_name = args.get(name_index)?.trim();
    let identifier = Regex::new(r"[A-Za-z_][A-Za-z0-9_:]*")
        .ok()?
        .find(raw_name)?
        .as_str();
    let name = identifier.rsplit("::").next()?.trim();

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn dedupe_delegate_classes(items: Vec<ClassInfo>) -> Vec<ClassInfo> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for item in items {
        let key = format!("{}:{}:{}", item.class_name, item.symbol_type, item.line);
        if seen.insert(key) {
            result.push(item);
        }
    }

    result
}

fn merge_reflected_type_fallbacks(root: Node, content_bytes: &[u8], classes: &mut Vec<ClassInfo>) {
    let Ok(content) = std::str::from_utf8(content_bytes) else {
        return;
    };

    for item in collect_reflected_type_fallbacks(root, content) {
        if let Some(existing) = classes.iter_mut().find(|class_info| {
            class_info.class_name == item.class_name
                && (
                    class_info.line == item.line
                        || (class_info.line >= item.line && class_info.line <= item.end_line)
                        || ranges_overlap(
                            class_info.range_start,
                            class_info.range_end,
                            item.range_start,
                            item.range_end,
                        )
                )
        }) {
            if is_reflected_symbol_type(&item.symbol_type)
                && !is_reflected_symbol_type(&existing.symbol_type)
            {
                existing.symbol_type = item.symbol_type.clone();
                existing.base_classes = item.base_classes.clone();
                existing.range_start = item.range_start;
                existing.range_end = item.range_end;
                existing.end_line = item.end_line;
                existing.is_interface = item.is_interface;
                if existing.namespace.is_none() {
                    existing.namespace = item.namespace.clone();
                }
            }

            continue;
        }

        classes.push(item);
    }
}

fn ranges_overlap(left_start: usize, left_end: usize, right_start: usize, right_end: usize) -> bool {
    left_start < right_end && right_start < left_end
}

fn collect_reflected_type_fallbacks(root: Node, content: &str) -> Vec<ClassInfo> {
    let bytes = content.as_bytes();
    let mut items = Vec::new();

    for (macro_name, symbol_type, keyword, is_interface) in [
        ("UCLASS", "UCLASS", "class", false),
        ("USTRUCT", "USTRUCT", "struct", false),
        ("UENUM", "UENUM", "enum", false),
        ("UINTERFACE", "UINTERFACE", "class", true),
    ] {
        let mut search_from = 0usize;

        while let Some(relative_start) = content[search_from..].find(macro_name) {
            let macro_start = search_from + relative_start;
            search_from = macro_start + macro_name.len();

            if !is_identifier_boundary(bytes, macro_start, macro_name.len()) {
                continue;
            }

            let mut cursor = macro_start + macro_name.len();
            cursor = skip_inline_whitespace(bytes, cursor);
            if bytes.get(cursor) != Some(&b'(') {
                continue;
            }

            let Some(close_paren) = find_matching_paren(content, cursor) else {
                continue;
            };

            let mut decl_start = skip_ws_and_comments(content, close_paren + 1);
            if decl_start >= bytes.len() {
                continue;
            }

            if symbol_type == "UENUM" {
                if content[decl_start..].starts_with("enum") {
                    decl_start += "enum".len();
                    decl_start = skip_inline_whitespace(bytes, decl_start);
                    if content[decl_start..].starts_with("class") {
                        decl_start += "class".len();
                    } else if content[decl_start..].starts_with("struct") {
                        decl_start += "struct".len();
                    }
                } else {
                    continue;
                }
            } else if content[decl_start..].starts_with(keyword) {
                decl_start += keyword.len();
            } else {
                continue;
            }

            decl_start = skip_inline_whitespace(bytes, decl_start);
            decl_start = skip_optional_api_macro(content, decl_start);
            decl_start = skip_inline_whitespace(bytes, decl_start);

            let Some((name, after_name)) = parse_identifier_like(content, decl_start) else {
                continue;
            };

            let after_name = skip_inline_whitespace(bytes, after_name);
            let Some(body_start) = content[after_name..].find('{').map(|offset| after_name + offset) else {
                continue;
            };

            let header_text = &content[decl_start..body_start];
            let base_classes = parse_base_classes(header_text);
            let range_end = find_matching_brace(content, body_start)
                .map(|close_brace| close_brace + 1)
                .unwrap_or(body_start + 1);
            let namespace = namespace_for_offset(root, content, macro_start);

            items.push(ClassInfo {
                class_name: strip_namespace(name).to_string(),
                namespace,
                base_classes,
                symbol_type: symbol_type.to_string(),
                line: line_number_for_offset(content, macro_start),
                end_line: line_number_for_offset(content, range_end.saturating_sub(1)),
                range_start: macro_start,
                range_end,
                members: Vec::new(),
                is_final: header_text.split_whitespace().any(|token| token == "final"),
                is_interface,
            });
        }
    }

    items
}

fn is_reflected_symbol_type(symbol_type: &str) -> bool {
    matches!(
        symbol_type,
        "UCLASS" | "USTRUCT" | "UENUM" | "UINTERFACE"
    )
}

fn is_identifier_boundary(bytes: &[u8], start: usize, len: usize) -> bool {
    let prev_ok = start == 0 || !is_identifier_byte(bytes[start - 1]);
    let next_index = start + len;
    let next_ok = next_index >= bytes.len() || !is_identifier_byte(bytes[next_index]);
    prev_ok && next_ok
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_inline_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn skip_ws_and_comments(content: &str, mut index: usize) -> usize {
    let bytes = content.as_bytes();

    loop {
        index = skip_inline_whitespace(bytes, index);

        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }

        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
            continue;
        }

        break;
    }

    index
}

fn skip_optional_api_macro(content: &str, index: usize) -> usize {
    let bytes = content.as_bytes();
    let Some((token, after_token)) = parse_identifier_like(content, index) else {
        return index;
    };

    if token.ends_with("_API") || token == "NO_API" {
        skip_inline_whitespace(bytes, after_token)
    } else {
        index
    }
}

fn parse_identifier_like(content: &str, index: usize) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    if index >= bytes.len() {
        return None;
    }

    let first = bytes[index];
    if !first.is_ascii_alphabetic() && first != b'_' {
        return None;
    }

    let mut end = index + 1;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':' {
            end += 1;
        } else {
            break;
        }
    }

    content.get(index..end).map(|text| (text, end))
}

fn parse_base_classes(header_text: &str) -> Vec<String> {
    let Some(colon_index) = header_text.find(':') else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let base_section = &header_text[colon_index + 1..];

    for raw_base in split_top_level_args(base_section) {
        let mut tokens = raw_base
            .split_whitespace()
            .filter(|token| {
                !matches!(
                    *token,
                    "public" | "protected" | "private" | "virtual" | "final"
                )
            })
            .collect::<Vec<_>>();

        if let Some(last) = tokens.pop() {
            let name = strip_namespace(last).trim();
            if !name.is_empty() {
                result.push(name.to_string());
            }
        }
    }

    result
}

fn find_matching_brace(content: &str, open_brace: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    let mut index = open_brace;

    while index < bytes.len() {
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }

        index += 1;
    }

    None
}

fn namespace_for_offset(root: Node, content: &str, offset: usize) -> Option<String> {
    let point = point_for_offset(content, offset);
    let node = root.descendant_for_point_range(point, point)?;
    get_namespace(&node, content.as_bytes())
}

fn point_for_offset(content: &str, offset: usize) -> tree_sitter::Point {
    let clamped = offset.min(content.len());
    let prefix = &content[..clamped];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let col = prefix
        .rsplit_once('\n')
        .map(|(_, tail)| tail.len())
        .unwrap_or(prefix.len());
    tree_sitter::Point::new(row, col)
}

fn line_number_for_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

/// Collect include paths.
/// 收集 include 路径。
fn collect_includes(
    cursor: &mut QueryCursor,
    include_query: &Query,
    root: Node,
    content_bytes: &[u8],
    includes: &mut Vec<String>,
) {
    let mut include_matches = cursor.matches(include_query, root, content_bytes);

    while let Some(m) = include_matches.next() {
        for cap in m.captures {
            if include_query.capture_names()[cap.index as usize] == "path" {
                let path = get_node_text(&cap.node, content_bytes)
                    .trim_matches('"')
                    .trim_matches('<')
                    .trim_matches('>')
                    .to_string();

                if !path.is_empty() {
                    includes.push(path);
                }
            }
        }
    }
}

/// Collect classes, members, calls, and enum items.
/// 收集类、成员、调用和枚举项。
fn collect_symbols(
    cursor: &mut QueryCursor,
    query: &Query,
    root: Node,
    content_bytes: &[u8],
    classes: &mut Vec<ClassInfo>,
    calls: &mut Vec<crate::types::CallInfo>,
    pending_members: &mut Vec<(MemberInfo, usize, usize)>,
) {
    let mut captures = cursor.captures(query, root, content_bytes);

    while let Some((m, capture_index)) = captures.next() {
        let capture = m.captures[*capture_index];
        let capture_name = query.capture_names()[capture.index as usize];
        let node = capture.node;

        match capture_name {
            "call_name" => collect_call(node, content_bytes, calls),

            "class_name" | "struct_name" | "enum_name" => {
                collect_class_like(node, capture_name, content_bytes, classes);
            }

            "base_class_name" => {
                collect_base_class(node, content_bytes, classes);
            }

            "func_node" | "decl_node" | "ufunc_node" | "field_node" => {
                if let Some(member) = build_member(node, capture_name, content_bytes, classes) {
                    pending_members.push((member, node.start_byte(), node.end_byte()));
                }
            }

            "enum_val_name" => {
                let member = MemberInfo {
                    name: get_node_text(&node, content_bytes).to_string(),
                    mem_type: "enum_item".to_string(),
                    flags: String::new(),
                    access: "public".to_string(),
                    line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    detail: None,
                    return_type: None,
                };

                pending_members.push((member, node.start_byte(), node.end_byte()));
            }

            _ => {}
        }
    }
}

/// Record one function or method call.
/// 记录一次函数或方法调用。
fn collect_call(
    node: Node,
    content_bytes: &[u8],
    calls: &mut Vec<crate::types::CallInfo>,
) {
    let name = get_node_text(&node, content_bytes).to_string();

    if !name.is_empty() {
        calls.push(crate::types::CallInfo {
            name,
            line: node.start_position().row + 1,
        });
    }
}

/// Build class / struct / enum records.
/// 构建 class / struct / enum 记录。
fn collect_class_like(
    node: Node,
    capture_name: &str,
    content_bytes: &[u8],
    classes: &mut Vec<ClassInfo>,
) {
    let Some(parent) = find_class_like_container(node) else {
        return;
    };

    let mut name = get_node_text(&node, content_bytes).to_string();
    let namespace = get_namespace(&parent, content_bytes);

    if capture_name == "enum_name" && name == "Type" {
        if let Some(ns) = &namespace {
            name = format!("{}::{}", ns, name);
        }
    }

    let symbol_type = match parent.kind() {
        "unreal_reflected_class_declaration" => "UCLASS",
        "unreal_reflected_struct_declaration" => "USTRUCT",
        "unreal_reflected_enum_declaration" => "UENUM",
        _ if capture_name == "struct_name" => "struct",
        _ if capture_name == "enum_name" => "enum",
        _ => "class",
    };

    classes.push(ClassInfo {
        class_name: name,
        namespace,
        base_classes: Vec::new(),
        symbol_type: symbol_type.to_string(),
        line: parent.start_position().row + 1,
        end_line: parent.end_position().row + 1,
        range_start: parent.start_byte(),
        range_end: parent.end_byte(),
        members: Vec::new(),
        is_final: node_has_token(parent, content_bytes, "final"),
        is_interface: node_has_child_kind(parent, "unreal_interface_macro"),
    });
}

fn find_class_like_container(node: Node) -> Option<Node> {
    let mut current = Some(node);

    while let Some(candidate) = current {
        if matches!(
            candidate.kind(),
            "class_specifier"
                | "struct_specifier"
                | "enum_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
                | "unreal_reflected_enum_declaration"
        ) {
            return candidate
                .child_by_field_name("body")
                .map(|_| candidate);
        }

        current = candidate.parent();
    }

    None
}

/// Attach a base class to the current class.
/// 给当前 class 挂父类。
fn collect_base_class(
    node: Node,
    content_bytes: &[u8],
    classes: &mut [ClassInfo],
) {
    let node_start = node.start_byte();

    let Some(cls) = classes.last_mut() else {
        return;
    };

    if node_start < cls.range_start || node_start > cls.range_end {
        return;
    }

    let mut name = get_node_text(&node, content_bytes).to_string();

    if let Some(idx) = name.rfind("::") {
        name = name[idx + 2..].to_string();
    }

    if !name.is_empty() && name != cls.class_name {
        cls.base_classes.push(name);
    }
}

/// Convert a function/declaration/field node into MemberInfo.
/// 把函数、声明、字段节点转换成 MemberInfo。
fn build_member(
    node: Node,
    capture_name: &str,
    content_bytes: &[u8],
    classes: &mut Vec<ClassInfo>,
) -> Option<MemberInfo> {
    let declarator = find_declarator_node(node)?;
    let member_identity = resolve_member_identity(declarator, content_bytes)?;

    let mut is_function = matches!(capture_name, "func_node" | "ufunc_node")
        || node.kind() == "unreal_function_declaration"
        || member_identity.is_function;

    let mut flags = Vec::new();

    // Node names intentionally match UTreeSitter.
    // 这里的节点名要和 UTreeSitter grammar 保持一致。
    if node_has_child_kind(node, "unreal_function_macro")
        || node_has_child_kind(node, "unreal_function_declaration")
        || node.kind() == "unreal_function_declaration"
    {
        flags.push("UFUNCTION");
        is_function = true;
    }

    if node_has_child_kind(node, "unreal_property_macro") {
        flags.push("UPROPERTY");
        is_function = false;
    }

    let scope_name = member_identity.scope_name;
    let access = if scope_name.is_some() && is_function {
        "impl".to_string()
    } else {
        infer_access(node, content_bytes)
    };

    let return_type = extract_return_or_property_type(node, declarator, content_bytes);

    let detail = if is_function {
        find_child_by_type(node, "parameter_list")
            .map(|params| get_node_text(&params, content_bytes).to_string())
    } else {
        None
    };

    let member_name = member_identity.name;

    if should_skip_member_name(&member_name) {
        return None;
    }

    let mut member = MemberInfo {
        name: member_name,
        mem_type: if is_function { "function" } else { "property" }.to_string(),
        flags: flags.join(" "),
        access,
        line: member_identity.line,
        end_line: node.end_position().row + 1,
        detail,
        return_type,
    };

    // Out-of-class implementation, e.g. UMyWidget::InitInfo.
    // 类外函数实现，例如 UMyWidget::InitInfo。
    if let Some(scope) = scope_name {
        let class_index = find_or_create_impl_class(classes, &scope);
        member.access = "impl".to_string();
        classes[class_index].members.push(member);
        return None;
    }

    Some(member)
}

/// Resolved declarator identity.
/// 从 declarator 里解析出来的成员身份。
struct MemberIdentity {
    name: String,
    scope_name: Option<String>,
    is_function: bool,
    line: usize,
}

/// Walk through nested declarators to find the real member name.
/// 穿过嵌套 declarator，找到真正的成员名。
fn resolve_member_identity(
    declarator: Node,
    content_bytes: &[u8],
) -> Option<MemberIdentity> {
    let mut current = declarator;
    let mut is_function = false;

    loop {
        match current.kind() {
            "identifier" | "field_identifier" => {
                return Some(MemberIdentity {
                    name: get_node_text(&current, content_bytes).to_string(),
                    scope_name: None,
                    is_function,
                    line: current.start_position().row + 1,
                });
            }

            "qualified_identifier" => {
                let scope_name = current
                    .child_by_field_name("scope")
                    .map(|scope| get_node_text(&scope, content_bytes).to_string());

                let name_node = current.child_by_field_name("name")?;
                let name = get_node_text(&name_node, content_bytes).to_string();

                if name.is_empty() {
                    return None;
                }

                return Some(MemberIdentity {
                    name,
                    scope_name,
                    is_function,
                    line: name_node.start_position().row + 1,
                });
            }

            "function_declarator" => {
                is_function = true;

                if let Some(next) = current.child_by_field_name("declarator") {
                    current = next;
                    continue;
                }

                return None;
            }

            "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator" => {
                if let Some(next) = current.child_by_field_name("declarator") {
                    current = next;
                    continue;
                }

                return None;
            }

            _ => return None,
        }
    }
}

/// Infer public/protected/private from preceding access specifiers.
/// 根据前面的 access specifier 推断 public/protected/private。
fn infer_access(node: Node, content_bytes: &[u8]) -> String {
    let mut access = "public".to_string();
    let mut current = node;

    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "field_declaration_list"
                | "class_specifier"
                | "struct_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
        ) {
            let mut cursor = parent.walk();

            for child in parent.children(&mut cursor) {
                if child.start_byte() >= current.start_byte() {
                    break;
                }

                if child.kind() == "access_specifier" {
                    access = get_node_text(&child, content_bytes)
                        .trim()
                        .trim_end_matches(':')
                        .to_ascii_lowercase();
                }
            }

            break;
        }

        current = parent;
    }

    access
}

/// Extract return type or property type from text before the declarator.
/// 从 declarator 前面的文本提取返回类型或属性类型。
fn extract_return_or_property_type(
    node: Node,
    declarator: Node,
    content_bytes: &[u8],
) -> Option<String> {
    let start = node.start_byte();
    let end = declarator.start_byte();

    if end <= start {
        return None;
    }

    let mut prefix = &content_bytes[start..end];

    // Skip macro argument text before the real type.
    // 跳过真正类型前面的宏参数文本。
    if let Some(idx) = prefix.iter().rposition(|&b| b == b')') {
        prefix = &prefix[idx + 1..];
    }

    let raw = std::str::from_utf8(prefix).unwrap_or("");
    let cleaned = clean_type_string(raw);

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Attach collected members to their smallest containing class.
/// 把收集到的成员挂到最小的包含它的 class 上。
fn attach_pending_members(
    classes: &mut [ClassInfo],
    pending_members: Vec<(MemberInfo, usize, usize)>,
) {
    for (member, member_start, member_end) in pending_members {
        let best_class = classes
            .iter()
            .enumerate()
            .filter(|(_, class_info)| {
                member_start >= class_info.range_start && member_end <= class_info.range_end
            })
            .min_by_key(|(_, class_info)| class_info.range_end - class_info.range_start)
            .map(|(index, _)| index);

        if let Some(index) = best_class {
            classes[index].members.push(member);
        }
    }
}

/// Get or create a synthetic class record for implementation-only files.
/// 获取或创建只在 cpp 实现文件里出现的虚拟 class 记录。
fn find_or_create_impl_class(classes: &mut Vec<ClassInfo>, scope: &str) -> usize {
    if let Some(index) = classes.iter().position(|class_info| class_info.class_name == scope) {
        return index;
    }

    classes.push(ClassInfo {
        class_name: scope.to_string(),
        namespace: None,
        base_classes: Vec::new(),
        symbol_type: "class".to_string(),
        line: 1,
        end_line: usize::MAX,
        range_start: 0,
        range_end: usize::MAX,
        members: Vec::new(),
        is_final: false,
        is_interface: false,
    });

    classes.len() - 1
}

fn should_skip_member_name(name: &str) -> bool {
    matches!(name, "virtual" | "static" | "void" | "const" | "class" | "struct")
}

fn get_node_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn strip_namespace(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name).trim()
}

/// Build namespace path from parent namespace/class/struct nodes.
/// 从父级 namespace/class/struct 节点构造命名空间路径。
fn get_namespace<'a>(node: &Node<'a>, source: &'a [u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut current = node.parent();

    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "namespace_definition"
                | "class_specifier"
                | "struct_specifier"
                | "unreal_reflected_class_declaration"
                | "unreal_reflected_struct_declaration"
        ) {
            if let Some(name) = parent.child_by_field_name("name") {
                parts.push(get_node_text(&name, source).to_string());
            }
        }

        current = parent.parent();
    }

    if parts.is_empty() {
        None
    } else {
        parts.reverse();
        Some(parts.join("::"))
    }
}

/// Depth-first search for a child node kind.
/// 深度优先查找指定 kind 的子节点。
fn find_child_by_type<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
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

/// Recursively check whether a node contains a child kind.
/// 递归检查节点是否包含某种子节点。
fn node_has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == kind || node_has_child_kind(child, kind) {
            return true;
        }
    }

    false
}

/// Check whether a token exists in node text.
/// 检查节点文本中是否包含某个 token。
fn node_has_token(node: Node, content_bytes: &[u8], token: &str) -> bool {
    get_node_text(&node, content_bytes)
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|part| part == token)
}

/// Find the first nested declarator field.
/// 查找第一个嵌套的 declarator 字段。
fn find_declarator_node<'a>(node: Node<'a>) -> Option<Node<'a>> {
    for index in 0..node.child_count() {
        if node.field_name_for_child(index as u32) == Some("declarator") {
            return node.child(index as u32);
        }

        if let Some(child) = node.child(index as u32) {
            if let Some(found) = find_declarator_node(child) {
                return Some(found);
            }
        }
    }

    None
}

/// Clean C++ type text down to the meaningful type name.
/// 把 C++ 类型文本清理成真正有意义的类型名。
pub(crate) fn clean_type_string(raw: &str) -> String {
    let regexes = get_clean_regexes();

    let mut clean = raw.trim().to_string();

    for keyword in &regexes.keywords {
        clean = keyword.replace_all(&clean, "").to_string();
    }

    clean = regexes.api.replace_all(&clean, "").to_string();
    clean = regexes.unreal_macros.replace_all(&clean, "").to_string();
    clean = clean.replace(';', "");
    clean = clean.replace(':', " : ");
    clean = regexes.whitespace.replace_all(&clean, " ").to_string();
    clean = clean.trim().to_string();

    if clean.contains('<') && clean.contains('>') {
        return clean;
    }

    clean
        .split_whitespace()
        .last()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        collect_delegate_definitions, collect_gameplay_tags, collect_macro_definitions,
        parse_content, QUERY_STR,
    };
    use tree_sitter::Query;

    #[test]
    fn collect_gameplay_tag_definitions_and_declarations() {
        let content = br#"
UE_DECLARE_GAMEPLAY_TAG_EXTERN(TAG_Weapon_Fire);
UE_DEFINE_GAMEPLAY_TAG(TAG_Weapon_Fire, "Weapon.Fire");
UE_DEFINE_GAMEPLAY_TAG_COMMENT(TAG_Status_Death, "Status.Death", "desc");
"#;

        let tags = collect_gameplay_tags(content);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].identifier, "TAG_Weapon_Fire");
        assert_eq!(tags[0].tag_path.as_deref(), Some("Weapon.Fire"));
        assert_eq!(tags[0].kind, "define");
        assert_eq!(tags[1].identifier, "TAG_Status_Death");
        assert_eq!(tags[1].tag_path.as_deref(), Some("Status.Death"));
    }

    #[test]
    fn collect_generic_macro_definitions() {
        let content = br#"
#define SIMPLE_MACRO 1
 # define FUNCTION_LIKE(Value) (Value)
"#;

        let macros = collect_macro_definitions(content);
        assert_eq!(macros.len(), 2);
        assert_eq!(macros[0].name, "SIMPLE_MACRO");
        assert_eq!(macros[1].name, "FUNCTION_LIKE");
    }

    #[test]
    fn collect_unreal_delegate_definitions() {
        let content = br#"
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FOnDamageTaken, float, Damage);
DECLARE_EVENT_OneParam(UMyWidget, FOnWidgetReady, UObject*);
DECLARE_DELEGATE_RetVal(bool, FCanOpenMenu);
"#;

        let delegates = collect_delegate_definitions(content);
        assert_eq!(delegates.len(), 3);
        assert_eq!(delegates[0].class_name, "FOnDamageTaken");
        assert_eq!(delegates[0].symbol_type, "dynamic_multicast_delegate");
        assert_eq!(delegates[1].class_name, "FOnWidgetReady");
        assert_eq!(delegates[1].symbol_type, "event");
        assert_eq!(delegates[2].class_name, "FCanOpenMenu");
        assert_eq!(delegates[2].symbol_type, "delegate");
    }

    #[test]
    fn parse_reflected_classes_with_unreal_macros_and_base_classes() {
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = Query::new(&language, QUERY_STR).expect("query should compile");
        let content = r#"
UCLASS(Blueprintable, MinimalAPI)
class UGameplayAbility : public UObject, public IGameplayTaskOwnerInterface
{
    GENERATED_UCLASS_BODY()
};

UCLASS(Blueprintable, meta = (ShowWorldContextPin), hidecategories = (Replication), MinimalAPI)
class AGameplayCueNotify_Actor : public AActor
{
    GENERATED_UCLASS_BODY()
};
"#;

        let (classes, _, _) =
            parse_content(content, "GameplayAbilityLike.h", &language, &query).expect("parse should succeed");

        let gameplay_ability = classes
            .iter()
            .find(|class_info| class_info.class_name == "UGameplayAbility")
            .expect("UGameplayAbility should be indexed");
        assert_eq!(gameplay_ability.symbol_type, "UCLASS");
        assert_eq!(
            gameplay_ability.base_classes,
            vec![
                "UObject".to_string(),
                "IGameplayTaskOwnerInterface".to_string()
            ]
        );

        let gameplay_cue = classes
            .iter()
            .find(|class_info| class_info.class_name == "AGameplayCueNotify_Actor")
            .expect("AGameplayCueNotify_Actor should be indexed");
        assert_eq!(gameplay_cue.symbol_type, "UCLASS");
        assert_eq!(gameplay_cue.base_classes, vec!["AActor".to_string()]);
    }

    #[test]
    fn parse_uinterface_and_preserve_reflected_access_levels() {
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = Query::new(&language, QUERY_STR).expect("query should compile");
        let content = r#"
namespace Outer
{
UINTERFACE(BlueprintType)
class SAMPLE_API UMyInterface : public UInterface
{
    GENERATED_BODY()
protected:
    UFUNCTION()
    void HiddenAction();
private:
    int32 HiddenValue;
};
}
"#;

        let (classes, _, _) =
            parse_content(content, "MyInterface.h", &language, &query).expect("parse should succeed");

        let interface_class = classes
            .iter()
            .find(|class_info| class_info.class_name == "UMyInterface")
            .expect("UMyInterface should be indexed");
        assert_eq!(interface_class.symbol_type, "UINTERFACE");
        assert_eq!(interface_class.namespace.as_deref(), Some("Outer"));
        assert_eq!(interface_class.base_classes, vec!["UInterface".to_string()]);
        assert!(interface_class.is_interface);

        let hidden_action = interface_class
            .members
            .iter()
            .find(|member| member.name == "HiddenAction")
            .expect("HiddenAction should be indexed");
        assert_eq!(hidden_action.access, "protected");

        let hidden_value = interface_class
            .members
            .iter()
            .find(|member| member.name == "HiddenValue")
            .expect("HiddenValue should be indexed");
        assert_eq!(hidden_value.access, "private");
    }

    #[test]
    fn parse_uenum_with_underlying_type_and_namespace() {
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = Query::new(&language, QUERY_STR).expect("query should compile");
        let content = r#"
namespace Demo
{
UENUM(BlueprintType)
enum class EAbilityPhase : uint8
{
    None,
    Start,
    End,
};
}
"#;

        let (classes, _, _) =
            parse_content(content, "AbilityPhase.h", &language, &query).expect("parse should succeed");

        let enum_info = classes
            .iter()
            .find(|class_info| class_info.class_name == "EAbilityPhase")
            .expect("EAbilityPhase should be indexed");
        assert_eq!(enum_info.symbol_type, "UENUM");
        assert_eq!(enum_info.namespace.as_deref(), Some("Demo"));
    }

    #[test]
    fn does_not_index_forward_decl_type_inside_template_argument_as_class() {
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = Query::new(&language, QUERY_STR).expect("query should compile");
        let content = r#"
USTRUCT(BlueprintType)
struct FZoraEquipValidationResult
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly)
    TSoftClassPtr<class UGameplayAbility> Ability;
};
"#;

        let (classes, _, _) =
            parse_content(content, "ZoraEquipValidationResult.h", &language, &query)
                .expect("parse should succeed");

        let struct_info = classes
            .iter()
            .find(|class_info| class_info.class_name == "FZoraEquipValidationResult")
            .expect("FZoraEquipValidationResult should be indexed");
        assert_eq!(struct_info.symbol_type, "USTRUCT");
        assert!(struct_info.members.iter().any(|member| member.name == "Ability"));

        assert!(
            classes
                .iter()
                .all(|class_info| class_info.class_name != "UGameplayAbility"),
            "forward-declared template argument should not be indexed as a class"
        );
    }
}

fn normalize_member_access(content_bytes: &[u8], classes: &mut [ClassInfo]) {
    let Ok(content) = std::str::from_utf8(content_bytes) else {
        return;
    };

    for class_info in classes {
        if class_info.members.is_empty()
            || !matches!(
                class_info.symbol_type.as_str(),
                "class" | "struct" | "UCLASS" | "USTRUCT" | "UINTERFACE"
            )
        {
            continue;
        }

        let Some(snippet) = content.get(class_info.range_start..class_info.range_end) else {
            continue;
        };

        let default_access = if matches!(class_info.symbol_type.as_str(), "struct" | "USTRUCT") {
            "public"
        } else {
            "private"
        };

        for member in &mut class_info.members {
            if member.access == "impl" {
                continue;
            }

            member.access = access_for_member_line(snippet, class_info.line, member.line, default_access).to_string();
        }
    }
}

fn access_for_member_line<'a>(
    snippet: &'a str,
    class_line: usize,
    member_line: usize,
    default_access: &'a str,
) -> &'a str {
    let mut access = default_access;

    for (index, raw_line) in snippet.lines().enumerate() {
        let line_number = class_line + index;
        if line_number > member_line {
            break;
        }

        let trimmed = raw_line.trim();
        if trimmed.starts_with("public:") {
            access = "public";
        } else if trimmed.starts_with("protected:") {
            access = "protected";
        } else if trimmed.starts_with("private:") {
            access = "private";
        }
    }

    access
}
