use super::condition_eval::evaluate_condition;
use super::tokenizer::parse_directive;
use super::{expand_include_operand, IncludeResolver, LineOrigin, MacroTable, PreprocessResult};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

const MAX_INCLUDE_DEPTH: usize = 32;

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
    let mut state = ExpansionState::with_capacity(source.len());
    let mut include_stack = HashSet::new();
    let mut pragma_once_files = HashSet::new();
    let mut include_guard_files = HashMap::new();
    let current_file = include_resolver.and_then(IncludeResolver::current_file_path);
    preprocess_source_inner(
        source,
        &mut macros,
        include_resolver,
        current_file,
        0,
        true,
        &mut include_stack,
        &mut pragma_once_files,
        &mut include_guard_files,
        &mut state,
    );
    state.finish()
}

struct ExpansionState {
    output: String,
    inactive_lines: HashSet<u32>,
    line_column_maps: Vec<Vec<u32>>,
    line_origins: Vec<LineOrigin>,
}

impl ExpansionState {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            output: String::with_capacity(capacity),
            inactive_lines: HashSet::new(),
            line_column_maps: Vec::new(),
            line_origins: Vec::new(),
        }
    }

    fn push_line(&mut self, text: &str, column_map: Vec<u32>, file_path: Option<&Path>, line: u32) {
        self.output.push_str(text);
        self.output.push('\n');
        self.line_column_maps.push(column_map);
        self.line_origins.push(LineOrigin {
            file_path: file_path.map(|path| path.to_string_lossy().replace('\\', "/")),
            line,
        });
    }

    fn finish(self) -> PreprocessResult {
        PreprocessResult {
            expanded_source: self.output,
            inactive_lines: self.inactive_lines,
            line_column_maps: self.line_column_maps,
            line_origins: self.line_origins,
        }
    }
}

fn preprocess_source_inner(
    source: &str,
    macros: &mut MacroTable,
    include_resolver: Option<&IncludeResolver>,
    current_file: Option<&Path>,
    include_depth: usize,
    track_inactive_lines: bool,
    include_stack: &mut HashSet<String>,
    pragma_once_files: &mut HashSet<String>,
    include_guard_files: &mut HashMap<String, String>,
    state: &mut ExpansionState,
) {
    let mut stack = Vec::<ConditionalFrame>::new();
    let current_file_key = current_file.map(include_identity);
    if let Some(file_key) = &current_file_key {
        if let Some(guard) = include_guard_files.get(file_key) {
            if macros.is_defined(guard) {
                return;
            }
        } else if let Some(guard) = detect_include_guard_macro(source) {
            include_guard_files.insert(file_key.clone(), guard);
        }
    }

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let directive = parse_directive(trimmed);
        let active = stack.iter().all(|frame| frame.current_active);

        if let Some(rest) = directive_body(directive.as_ref(), "if") {
            let condition = active && evaluate_condition(rest, &macros, include_resolver);
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "ifdef") {
            let condition = active && macros.is_defined(rest.trim());
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "ifndef") {
            let condition = active && !macros.is_defined(rest.trim());
            stack.push(ConditionalFrame {
                parent_active: active,
                current_active: condition,
                branch_taken: condition,
            });
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "elif") {
            if let Some(top) = stack.last_mut() {
                let cond = !top.branch_taken
                    && top.parent_active
                    && evaluate_condition(rest, &macros, include_resolver);
                top.current_active = cond;
                top.branch_taken |= cond;
            }
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if directive.as_ref().is_some_and(|directive| directive.name == "else") {
            if let Some(top) = stack.last_mut() {
                top.current_active = top.parent_active && !top.branch_taken;
                top.branch_taken = true;
            }
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if directive.as_ref().is_some_and(|directive| directive.name == "endif") {
            stack.pop();
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }

        let active = stack.iter().all(|frame| frame.current_active);
        if directive
            .as_ref()
            .is_some_and(|directive| directive.name == "pragma" && directive.body.trim() == "once")
        {
            if active && let Some(current_file_key) = &current_file_key {
                pragma_once_files.insert(current_file_key.clone());
            }
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "define") {
            if active {
                macros.define_from_directive(rest);
            }
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "undef") {
            if active {
                macros.undefine(rest.trim());
            }
            state.push_line("", vec![0], current_file, line_index as u32);
            continue;
        }
        if let Some(rest) = directive_body(directive.as_ref(), "include") {
            state.push_line("", vec![0], current_file, line_index as u32);

            if active
                && include_depth < MAX_INCLUDE_DEPTH
                && let Some(resolver) = include_resolver
                && let Some(include_path) =
                    resolver.resolve_include_path(&expand_include_operand(rest.trim(), macros))
            {
                let include_key = include_identity(include_path.as_path());
                if pragma_once_files.contains(&include_key) {
                    continue;
                }
                if include_stack.insert(include_key.clone()) {
                    if let Ok(include_source) = fs::read_to_string(&include_path) {
                        let child_resolver = resolver.for_included_file(include_path.clone());
                        preprocess_source_inner(
                            &include_source,
                            macros,
                            Some(&child_resolver),
                            Some(include_path.as_path()),
                            include_depth + 1,
                            false,
                            include_stack,
                            pragma_once_files,
                            include_guard_files,
                            state,
                        );
                    }
                    include_stack.remove(&include_key);
                }
            }
            continue;
        }

        if active {
            let (expanded, column_map) = macros.expand_line_with_map(line);
            state.push_line(&expanded, column_map, current_file, line_index as u32);
        } else {
            if track_inactive_lines {
                state.inactive_lines.insert(line_index as u32);
            }
            state.push_line("", vec![0], current_file, line_index as u32);
        }
    }
}

fn directive_body<'a>(
    directive: Option<&super::tokenizer::Directive<'a>>,
    expected: &str,
) -> Option<&'a str> {
    let directive = directive?;
    (directive.name == expected).then_some(directive.body)
}

fn include_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

fn detect_include_guard_macro(source: &str) -> Option<String> {
    let mut candidate = None::<String>;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }

        let directive = parse_directive(trimmed)?;
        match candidate.as_ref() {
            None if directive.name == "ifndef" => {
                let guard = directive.body.trim();
                if guard.is_empty() {
                    return None;
                }
                candidate = Some(guard.to_string());
            }
            None if directive.name == "if" => {
                candidate = parse_defined_include_guard(directive.body);
                if candidate.is_none() {
                    return None;
                }
            }
            Some(expected) if directive.name == "define" => {
                let defined = directive
                    .body
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim();
                return (defined == expected).then(|| expected.clone());
            }
            _ => return None,
        }
    }

    None
}

fn parse_defined_include_guard(body: &str) -> Option<String> {
    let compact = body.chars().filter(|ch| !ch.is_whitespace()).collect::<String>();
    compact
        .strip_prefix("!defined(")
        .and_then(|rest| rest.strip_suffix(')'))
        .filter(|guard| !guard.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            compact
                .strip_prefix("!defined")
                .filter(|guard| !guard.is_empty())
                .map(ToString::to_string)
        })
}
