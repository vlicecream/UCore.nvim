use super::condition_eval::evaluate_condition;
use super::{IncludeResolver, MacroTable, PreprocessResult};

#[derive(Clone, Copy, Debug)]
struct ConditionalFrame {
    parent_active: bool,
    current_active: bool,
    branch_taken: bool,
}

pub fn preprocess_source(source: &str, base_macros: &MacroTable) -> PreprocessResult {
    preprocess_source_with_resolver(source, base_macros, None)
}

pub fn preprocess_source_with_resolver(
    source: &str,
    base_macros: &MacroTable,
    include_resolver: Option<&IncludeResolver>,
) -> PreprocessResult {
    let mut macros = base_macros.clone();
    let mut inactive_lines = std::collections::HashSet::new();
    let mut output = String::with_capacity(source.len());
    let mut line_column_maps = Vec::new();
    let mut stack = Vec::<ConditionalFrame>::new();

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let active = stack.iter().all(|frame| frame.current_active);

        if let Some(rest) = directive_body(trimmed, "if ") {
            let condition = active && evaluate_condition(rest, &macros, include_resolver);
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "ifdef ") {
            let condition = active && macros.is_defined(rest.trim());
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "ifndef ") {
            let condition = active && !macros.is_defined(rest.trim());
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "elif ") {
            if let Some(top) = stack.last_mut() {
                let cond = !top.branch_taken
                    && top.parent_active
                    && evaluate_condition(rest, &macros, include_resolver);
                top.current_active = cond;
                top.branch_taken |= cond;
            }
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if trimmed.starts_with("#else") {
            if let Some(top) = stack.last_mut() {
                top.current_active = top.parent_active && !top.branch_taken;
                top.branch_taken = true;
            }
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if trimmed.starts_with("#endif") {
            stack.pop();
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }

        let active = stack.iter().all(|frame| frame.current_active);
        if let Some(rest) = directive_body(trimmed, "define ") {
            if active {
                macros.define_from_directive(rest);
            }
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "undef ") {
            if active {
                macros.undefine(rest.trim());
            }
            line_column_maps.push(vec![0]);
            output.push('\n');
            continue;
        }

        if active {
            let (expanded, column_map) = macros.expand_line_with_map(line);
            output.push_str(&expanded);
            line_column_maps.push(column_map);
        } else {
            inactive_lines.insert(line_index as u32);
            line_column_maps.push(vec![0]);
        }
        output.push('\n');
    }

    PreprocessResult {
        expanded_source: output,
        inactive_lines,
        line_column_maps,
    }
}

fn directive_body<'a>(trimmed: &'a str, directive: &str) -> Option<&'a str> {
    let body = trimmed.strip_prefix('#')?.trim_start();
    body.strip_prefix(directive)
}
