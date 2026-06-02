use super::condition_eval::evaluate_condition;
use super::{MacroTable, PreprocessResult};

#[derive(Clone, Copy, Debug)]
struct ConditionalFrame {
    parent_active: bool,
    current_active: bool,
    branch_taken: bool,
}

pub fn preprocess_source(source: &str, base_macros: &MacroTable) -> PreprocessResult {
    let mut macros = base_macros.clone();
    let mut inactive_lines = std::collections::HashSet::new();
    let mut output = String::with_capacity(source.len());
    let mut stack = Vec::<ConditionalFrame>::new();

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let active = stack.iter().all(|frame| frame.current_active);

        if let Some(rest) = directive_body(trimmed, "if ") {
            let condition = active && evaluate_condition(rest, &macros);
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
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
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "elif ") {
            if let Some(top) = stack.last_mut() {
                let cond = !top.branch_taken && top.parent_active && evaluate_condition(rest, &macros);
                top.current_active = cond;
                top.branch_taken |= cond;
            }
            output.push('\n');
            continue;
        }
        if trimmed.starts_with("#else") {
            if let Some(top) = stack.last_mut() {
                top.current_active = top.parent_active && !top.branch_taken;
                top.branch_taken = true;
            }
            output.push('\n');
            continue;
        }
        if trimmed.starts_with("#endif") {
            stack.pop();
            output.push('\n');
            continue;
        }

        let active = stack.iter().all(|frame| frame.current_active);
        if let Some(rest) = directive_body(trimmed, "define ") {
            if active {
                macros.define_from_directive(rest);
            }
            output.push('\n');
            continue;
        }
        if let Some(rest) = directive_body(trimmed, "undef ") {
            if active {
                macros.undefine(rest.trim());
            }
            output.push('\n');
            continue;
        }

        if active {
            output.push_str(&macros.expand_line(line));
        } else {
            inactive_lines.insert(line_index as u32);
        }
        output.push('\n');
    }

    PreprocessResult {
        expanded_source: output,
        inactive_lines,
    }
}

fn directive_body<'a>(trimmed: &'a str, directive: &str) -> Option<&'a str> {
    let body = trimmed.strip_prefix('#')?.trim_start();
    body.strip_prefix(directive)
}
