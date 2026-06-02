use std::collections::{HashMap, HashSet};

use super::{DiagnosticItem, DiagnosticSeverity, FinalizeRules};

pub(crate) fn finalize_diagnostics(
    items: Vec<DiagnosticItem>,
    content: &str,
    file_path: Option<&str>,
    rules: &FinalizeRules,
) -> Vec<DiagnosticItem> {
    let total_lines = content.lines().count().max(1) as u32;
    let suppressed_if_zero = rules
        .suppress_inside_preproc_if_zero
        .then(|| suppressed_lines_inside_preproc(content))
        .unwrap_or_default();

    let mut deduped = HashMap::<(Option<String>, u32, u32, &'static str), DiagnosticItem>::new();
    for item in items {
        if should_suppress_tail_line(&item, total_lines, rules.suppress_tail_lines) {
            continue;
        }
        if suppressed_if_zero.contains(&item.line) {
            continue;
        }

        let key = (
            item.file_path.clone(),
            item.line,
            dedupe_bucket(item.character, rules.dedupe_window_cols),
            item.code,
        );
        match deduped.get(&key) {
            Some(existing) if severity_rank(&existing.severity) >= severity_rank(&item.severity) => {}
            _ => {
                deduped.insert(key, item);
            }
        }
    }

    let mut items = deduped.into_values().collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.character.cmp(&right.character))
            .then_with(|| left.code.cmp(right.code))
    });

    let mut limited = Vec::new();
    let mut line_counts = HashMap::<(Option<String>, u32), usize>::new();
    for item in items {
        let line_key = (item.file_path.clone(), item.line);
        let entry = line_counts.entry(line_key).or_default();
        if *entry >= rules.max_per_line {
            continue;
        }
        *entry += 1;
        limited.push(item);
    }

    if limited.len() > rules.max_per_file {
        let shown = rules.max_per_file;
        let hidden = limited.len().saturating_sub(shown);
        limited.truncate(shown);
        limited.push(
            DiagnosticItem::new(
                file_path,
                total_lines.saturating_sub(1),
                0,
                DiagnosticSeverity::Information,
                "UCore",
                "UCORE-FIN-001",
                format!("More diagnostics below; {shown} shown, {hidden} suppressed."),
            )
            .with_end(total_lines.saturating_sub(1), 1),
        );
    }

    limited
}

fn should_suppress_tail_line(item: &DiagnosticItem, total_lines: u32, suppress_tail_lines: usize) -> bool {
    if suppress_tail_lines == 0 {
        return false;
    }

    let threshold = total_lines.saturating_sub(suppress_tail_lines as u32);
    item.line >= threshold
}

fn dedupe_bucket(column: u32, window: u32) -> u32 {
    if window <= 1 {
        column
    } else {
        column / window
    }
}

fn severity_rank(severity: &DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 4,
        DiagnosticSeverity::Warning => 3,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 1,
    }
}

fn suppressed_lines_inside_preproc(content: &str) -> HashSet<u32> {
    crate::preproc::preprocess_source(content, &crate::preproc::default_macro_table()).inactive_lines
}
