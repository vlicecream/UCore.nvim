use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;
use serde::Deserialize;

use crate::diagnostics::{BadCharactersRules, DiagnosticItem, DiagnosticSeverity};

static UNICODE_RULES: OnceLock<UnicodePunctPairs> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Default)]
struct UnicodePunctPairs {
    #[serde(default)]
    full_width: HashMap<String, String>,
    #[serde(default)]
    invisible: HashMap<String, String>,
}

pub(crate) fn collect(
    content: &str,
    file_path: Option<&str>,
    parsed_root: Option<tree_sitter::Node>,
    rules: &BadCharactersRules,
) -> Result<Vec<DiagnosticItem>> {
    let unicode_rules = UNICODE_RULES.get_or_init(|| {
        toml::from_str(include_str!("../../../data/unicode_punct_pairs.toml")).unwrap_or_default()
    });
    let mut items = Vec::new();
    let line_starts = line_start_offsets(content);

    for (byte_index, ch) in content.char_indices() {
        let mut replacement = None;
        let mut severity = DiagnosticSeverity::from(rules.full_width_severity);
        let mut code = "UECPP-CHR-001";
        let mut message = String::new();
        let key = ch.to_string();

        if let Some(ascii) = unicode_rules.full_width.get(&key) {
            replacement = Some(ascii.clone());
            message = format!("Full-width punctuation '{}' should be '{}'.", ch, ascii);
        } else if let Some(name) = unicode_rules.invisible.get(&key) {
            severity = DiagnosticSeverity::from(rules.invisible_severity);
            code = "UECPP-CHR-002";
            message = format!("Invisible character {} is not allowed here.", name);
        }

        if message.is_empty() {
            continue;
        }

        let end = byte_index + ch.len_utf8();
        if is_ignored_character_context(parsed_root, byte_index, end) {
            continue;
        }

        let (line, col) = line_and_column_for_byte(content, &line_starts, byte_index);
        let end_col = col + ch.len_utf8() as u32;
        let mut item = DiagnosticItem::new(
            file_path,
            line,
            col,
            severity,
            "UCore",
            code,
            message,
        )
        .with_end(line, end_col);

        if let Some(ascii) = replacement {
            item.message.push_str(&format!(" Replace it with '{}'.", ascii));
        }

        items.push(item);
    }

    Ok(items)
}

fn is_ignored_character_context(
    parsed_root: Option<tree_sitter::Node>,
    start_byte: usize,
    end_byte: usize,
) -> bool {
    let Some(root) = parsed_root else {
        return false;
    };
    let Some(node) = root.descendant_for_byte_range(start_byte, end_byte) else {
        return false;
    };

    let mut current = Some(node);
    while let Some(node) = current {
        if matches!(
            node.kind(),
            "string_literal"
                | "char_literal"
                | "raw_string_literal"
                | "comment"
                | "preproc_arg"
        ) {
            return true;
        }
        current = node.parent();
    }

    false
}

fn line_start_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

fn line_and_column_for_byte(content: &str, line_starts: &[usize], byte_index: usize) -> (u32, u32) {
    let line = line_starts.partition_point(|start| *start <= byte_index).saturating_sub(1);
    let line_start = line_starts.get(line).copied().unwrap_or(0);
    let column = content[line_start..byte_index].chars().count() as u32;
    (line as u32, column)
}
