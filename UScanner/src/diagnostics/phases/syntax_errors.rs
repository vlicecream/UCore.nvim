use anyhow::Result;
use std::collections::HashSet;

use crate::diagnostics::{DiagnosticItem, DiagnosticSeverity, SyntaxErrorsRules};

pub(crate) fn collect(
    content: &str,
    file_path: Option<&str>,
    parsed_root: Option<tree_sitter::Node>,
    rules: &SyntaxErrorsRules,
    preproc_config_file: &str,
) -> Result<Vec<DiagnosticItem>> {
    let Some(root) = parsed_root else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    let total_lines = content.lines().count();
    let tail_threshold = total_lines.saturating_sub(rules.suppress_tail_lines);
    let suppressed_lines = if rules.skip_preproc_if_zero {
        suppressed_lines_inside_preproc_if_zero(content, file_path, preproc_config_file)
    } else {
        HashSet::new()
    };

    collect_syntax_items(
        root,
        content,
        file_path,
        rules,
        tail_threshold,
        &suppressed_lines,
        &mut items,
    );
    Ok(items)
}

fn collect_syntax_items(
    node: tree_sitter::Node,
    content: &str,
    file_path: Option<&str>,
    rules: &SyntaxErrorsRules,
    tail_threshold: usize,
    suppressed_lines: &HashSet<u32>,
    items: &mut Vec<DiagnosticItem>,
) {
    let row = node.start_position().row as usize;
    if row >= tail_threshold
        || suppressed_lines.contains(&(row as u32))
        || is_preprocessor_directive_line(content, row)
    {
        return;
    }

    if node.is_missing() {
        items.push(build_missing_item(node, content, file_path, rules));
    } else if node.is_error() && rules.report_error_node {
        items.push(build_error_item(node, content, file_path, rules));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_syntax_items(
            child,
            content,
            file_path,
            rules,
            tail_threshold,
            suppressed_lines,
            items,
        );
    }
}

fn is_preprocessor_directive_line(content: &str, row: usize) -> bool {
    content
        .lines()
        .nth(row)
        .map(str::trim_start)
        .is_some_and(|line| line.starts_with('#'))
}

fn build_missing_item(
    node: tree_sitter::Node,
    content: &str,
    file_path: Option<&str>,
    rules: &SyntaxErrorsRules,
) -> DiagnosticItem {
    let token = grammar_token(node);
    let point = node.start_position();
    let message = match token.as_str() {
        ";" => "Missing ';' after declaration.".to_string(),
        ")" => {
            if let Some(open_line) = find_open_delimiter_line(content, node.start_byte(), '(', ')') {
                format!("Missing ')'; opened at line {}.", open_line + 1)
            } else {
                "Missing ')'.".to_string()
            }
        }
        "]" => {
            if let Some(open_line) = find_open_delimiter_line(content, node.start_byte(), '[', ']') {
                format!("Missing ']'; opened at line {}.", open_line + 1)
            } else {
                "Missing ']'.".to_string()
            }
        }
        "}" => {
            if let Some(open_line) = find_open_delimiter_line(content, node.start_byte(), '{', '}') {
                format!("Missing '}}'; opened at line {}.", open_line + 1)
            } else {
                "Missing '}'.".to_string()
            }
        }
        _ => format!("Missing '{}'.", token),
    };

    DiagnosticItem::new(
        file_path,
        point.row as u32,
        point.column as u32,
        DiagnosticSeverity::from(rules.severity_missing),
        "UCore",
        match token.as_str() {
            ";" => "UECPP-SYN-001",
            ")" | "]" | "}" => "UECPP-SYN-002",
            _ => "UECPP-SYN-003",
        },
        message,
    )
    .with_end(point.row as u32, point.column as u32 + 1)
}

fn build_error_item(
    node: tree_sitter::Node,
    content: &str,
    file_path: Option<&str>,
    rules: &SyntaxErrorsRules,
) -> DiagnosticItem {
    let point = node.start_position();
    let first_child = first_error_child_text(node, content);
    let (code, message) = if let Some(token) = first_child {
        (
            "UECPP-SYN-010",
            format!("Unexpected '{}' (syntax error).", token),
        )
    } else {
        ("UECPP-SYN-011", "Syntax error here.".to_string())
    };

    DiagnosticItem::new(
        file_path,
        point.row as u32,
        point.column as u32,
        DiagnosticSeverity::from(rules.severity_error),
        "UCore",
        code,
        message,
    )
    .with_end(node.end_position().row as u32, node.end_position().column as u32)
}

fn grammar_token(node: tree_sitter::Node) -> String {
    let token = node.grammar_name();
    if token.is_empty() {
        node.kind().to_string()
    } else {
        token.to_string()
    }
}

fn first_error_child_text(node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find_map(|child| {
            let text = text_for_node(child, content);
            (!text.is_empty()).then_some(text)
        })
}

fn text_for_node(node: tree_sitter::Node, content: &str) -> String {
    let range = node.byte_range();
    if range.end > content.len()
        || !content.is_char_boundary(range.start)
        || !content.is_char_boundary(range.end)
    {
        return String::new();
    }
    content[range.start..range.end]
        .trim()
        .chars()
        .take(32)
        .collect::<String>()
}

fn find_open_delimiter_line(content: &str, limit_byte: usize, open: char, close: char) -> Option<usize> {
    let mut stack = Vec::<usize>::new();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;
    let bytes = content.as_bytes();
    let mut row = 0usize;
    let mut index = 0usize;

    while index < bytes.len() && index < limit_byte {
        let ch = bytes[index] as char;
        let next = bytes.get(index + 1).copied().map(char::from);

        if ch == '\n' {
            row += 1;
            in_line_comment = false;
            escape = false;
            index += 1;
            continue;
        }

        if in_line_comment {
            index += 1;
            continue;
        }
        if in_block_comment {
            if ch == '*' && next == Some('/') {
                in_block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if in_string {
            if !escape && ch == '"' {
                in_string = false;
            }
            escape = ch == '\\' && !escape;
            index += 1;
            continue;
        }
        if in_char {
            if !escape && ch == '\'' {
                in_char = false;
            }
            escape = ch == '\\' && !escape;
            index += 1;
            continue;
        }

        if ch == '/' && next == Some('/') {
            in_line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && next == Some('*') {
            in_block_comment = true;
            index += 2;
            continue;
        }
        if ch == '"' {
            in_string = true;
            index += 1;
            continue;
        }
        if ch == '\'' {
            in_char = true;
            index += 1;
            continue;
        }

        if ch == open {
            stack.push(row);
        } else if ch == close {
            stack.pop();
        }

        index += 1;
    }

    stack.pop()
}

fn suppressed_lines_inside_preproc_if_zero(
    content: &str,
    file_path: Option<&str>,
    preproc_config_file: &str,
) -> HashSet<u32> {
    let macros = crate::preproc::default_macro_table_for_file(preproc_config_file);
    let resolver = crate::preproc::default_include_resolver_for_file(preproc_config_file, file_path);
    crate::preproc::preprocess_source_cached_with_resolver(content, &macros, Some(&resolver), file_path)
        .inactive_lines
}
