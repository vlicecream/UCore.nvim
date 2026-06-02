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
        .then(|| suppressed_lines_inside_if_zero(content))
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

#[derive(Clone, Copy)]
enum PreprocFrameKind {
    IfZero,
    Other,
}

#[derive(Clone, Copy)]
struct PreprocFrame {
    kind: PreprocFrameKind,
    suppressing: bool,
}

fn suppressed_lines_inside_if_zero(content: &str) -> HashSet<u32> {
    let mut suppressed = HashSet::new();
    let mut stack = Vec::<PreprocFrame>::new();

    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#if") {
            let rest = rest.trim_start();
            let frame = if rest == "0" {
                PreprocFrame {
                    kind: PreprocFrameKind::IfZero,
                    suppressing: true,
                }
            } else {
                PreprocFrame {
                    kind: PreprocFrameKind::Other,
                    suppressing: parent_suppressed(&stack),
                }
            };
            stack.push(frame);
            continue;
        }
        if trimmed.starts_with("#ifdef") || trimmed.starts_with("#ifndef") {
            stack.push(PreprocFrame {
                kind: PreprocFrameKind::Other,
                suppressing: parent_suppressed(&stack),
            });
            continue;
        }
        if trimmed.starts_with("#elif") {
            let parent = parent_suppressed_without_top(&stack);
            if let Some(top) = stack.last_mut() {
                top.suppressing = match top.kind {
                    PreprocFrameKind::IfZero => {
                        let expr = trimmed.trim_start_matches("#elif").trim();
                        !matches!(expr, "1" | "true" | "TRUE")
                    }
                    PreprocFrameKind::Other => parent,
                };
            }
            continue;
        }
        if trimmed.starts_with("#else") {
            let parent = parent_suppressed_without_top(&stack);
            if let Some(top) = stack.last_mut() {
                top.suppressing = match top.kind {
                    PreprocFrameKind::IfZero => !top.suppressing && !parent,
                    PreprocFrameKind::Other => parent,
                };
            }
            continue;
        }
        if trimmed.starts_with("#endif") {
            stack.pop();
            continue;
        }

        if stack.iter().any(|frame| frame.suppressing) {
            suppressed.insert(index as u32);
        }
    }

    suppressed
}

fn parent_suppressed(stack: &[PreprocFrame]) -> bool {
    stack.iter().any(|frame| frame.suppressing)
}

fn parent_suppressed_without_top(stack: &[PreprocFrame]) -> bool {
    stack
        .split_last()
        .map(|(_, rest)| rest.iter().any(|frame| frame.suppressing))
        .unwrap_or(false)
}
