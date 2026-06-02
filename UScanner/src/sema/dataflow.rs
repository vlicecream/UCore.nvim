use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;
use tree_sitter::Node;

use super::cfg::{build_cfg, Cfg};
use super::symbol::{Storage, SymbolId, SymbolKind};
use super::SemaContext;

static RAII_PREFIXES: OnceLock<RaiiPrefixesFile> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct DataflowIssue {
    pub code: &'static str,
    pub message: String,
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

#[derive(Debug, Clone)]
pub struct DataflowResult {
    pub _cfg: Cfg,
    pub issues: Vec<DataflowIssue>,
}

#[derive(Debug, Clone)]
struct LocalVarState {
    name: String,
    decl_line: u32,
    decl_col: u32,
    decl_end_col: u32,
    initialized: bool,
    used: bool,
    track_unused: bool,
}

#[derive(Debug, Deserialize, Default)]
struct RaiiPrefixesFile {
    #[serde(default)]
    prefixes: Vec<String>,
}

pub fn analyze(root: Node, source: &str, sema: &SemaContext) -> Vec<DataflowResult> {
    let mut results = Vec::new();
    collect_functions(root, &mut |function_node| {
        results.push(analyze_function(function_node, source, sema));
    });
    results
}

fn analyze_function(function_node: Node, source: &str, sema: &SemaContext) -> DataflowResult {
    let cfg = build_cfg(function_node);
    let mut issues = Vec::new();
    let mut states = HashMap::<SymbolId, LocalVarState>::new();
    let mut order = Vec::<SymbolId>::new();

    if let Some(body) = find_descendant(function_node, "compound_statement") {
        let mut active_stack = Vec::<HashSet<SymbolId>>::new();
        analyze_block(
            body,
            source,
            sema,
            &mut states,
            &mut order,
            &mut active_stack,
            &mut issues,
        );
    }

    for symbol_id in order {
        let Some(state) = states.get(&symbol_id) else {
            continue;
        };
        if state.track_unused && !state.used {
            issues.push(DataflowIssue {
                code: "UECPP-DF-001",
                message: format!("Local variable {} is never used.", state.name),
                line: state.decl_line,
                character: state.decl_col,
                end_line: state.decl_line,
                end_character: state.decl_end_col,
            });
        }
    }

    DataflowResult { _cfg: cfg, issues }
}

fn analyze_block(
    block: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    active_stack.push(HashSet::new());

    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        analyze_node(child, source, sema, states, order, active_stack, issues);
    }

    active_stack.pop();
}

fn analyze_node(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    match node.kind() {
        "compound_statement" => analyze_block(node, source, sema, states, order, active_stack, issues),
        "declaration" => {
            handle_declaration(node, source, sema, states, order, active_stack, issues);
            analyze_declaration_initializer(node, source, sema, states, order, active_stack, issues);
        }
        "parameter_declaration" => {
            handle_parameter(node, source, sema, states, active_stack, issues);
        }
        "assignment_expression" => {
            handle_assignment(node, source, sema, states, order, active_stack, issues);
        }
        "if_statement" | "for_statement" | "while_statement" | "switch_statement" => {
            walk_children(node, source, sema, states, order, active_stack, issues);
        }
        "identifier" => {
            handle_identifier_use(node, source, sema, states, issues);
        }
        _ => walk_children(node, source, sema, states, order, active_stack, issues),
    }
}

fn handle_declaration(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut [HashSet<SymbolId>],
    issues: &mut Vec<DataflowIssue>,
) {
    let Some(name_node) = node.child_by_field_name("declarator").and_then(find_name_node) else {
        return;
    };
    let name = node_text(name_node, source).trim().to_string();
    if name.is_empty() {
        return;
    }

    let Some(symbol_id) = local_symbol_for_name(sema, name_node, &name) else {
        return;
    };

    if let Some(parent_symbol_id) = lookup_active_name_in_parents(sema, name_node, &name) {
        if let Some(existing) = sema.symbols.get(parent_symbol_id) {
            if matches!(existing.kind, SymbolKind::Variable { .. }) {
                issues.push(DataflowIssue {
                    code: "UECPP-DF-003",
                    message: format!("Local variable {} shadows an outer declaration.", name),
                    line: name_node.start_position().row as u32,
                    character: name_node.start_position().column as u32,
                    end_line: name_node.end_position().row as u32,
                    end_character: name_node.end_position().column as u32,
                });
            }
        }
    }

    let initialized = declaration_has_initializer(node, source);
    let type_name = node
        .child_by_field_name("type")
        .map(|type_node| crate::parser::cpp::clean_type_string(node_text(type_node, source)));
    let track_unused = !name.starts_with('_')
        && !is_raii_type(type_name.as_deref().unwrap_or_default());

        states.insert(
            symbol_id,
            LocalVarState {
                name: name.clone(),
                decl_line: name_node.start_position().row as u32,
                decl_col: name_node.start_position().column as u32,
                decl_end_col: name_node.end_position().column as u32,
                initialized,
                used: false,
                track_unused,
        },
    );
    order.push(symbol_id);
    if let Some(active) = active_stack.last_mut() {
        active.insert(symbol_id);
    }
}

fn handle_parameter(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    active_stack: &mut [HashSet<SymbolId>],
    issues: &mut Vec<DataflowIssue>,
) {
    let Some(name_node) = node.child_by_field_name("declarator").and_then(find_name_node) else {
        return;
    };
    let name = node_text(name_node, source).trim().to_string();
    if name.is_empty() {
        return;
    }

    if let Some(parent_symbol_id) = lookup_active_name_in_parents(sema, name_node, &name) {
        if let Some(existing) = sema.symbols.get(parent_symbol_id) {
            if matches!(existing.kind, SymbolKind::Variable { .. }) {
                issues.push(DataflowIssue {
                    code: "UECPP-DF-003",
                    message: format!("Parameter {} shadows an outer declaration.", name),
                    line: name_node.start_position().row as u32,
                    character: name_node.start_position().column as u32,
                    end_line: name_node.end_position().row as u32,
                    end_character: name_node.end_position().column as u32,
                });
            }
        }
    }

    if let Some(symbol_id) = local_symbol_for_name(sema, name_node, &name) {
        states.insert(
            symbol_id,
            LocalVarState {
                name,
                decl_line: name_node.start_position().row as u32,
                decl_col: name_node.start_position().column as u32,
                decl_end_col: name_node.end_position().column as u32,
                initialized: true,
                used: false,
                track_unused: false,
            },
        );
        if let Some(active) = active_stack.last_mut() {
            active.insert(symbol_id);
        }
    }
}

fn handle_assignment(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    let lhs = node.child_by_field_name("left").or_else(|| node.child(0));
    let rhs = node.child_by_field_name("right").or_else(|| node.child(2));

    if let Some(rhs) = rhs {
        analyze_node(rhs, source, sema, states, order, active_stack, issues);
    }

    if let Some(lhs) = lhs {
        if lhs.kind() == "identifier" {
            let name = node_text(lhs, source).trim();
            if let Some(symbol_id) = sema.resolve_symbol_at_node(lhs, name) {
                if let Some(state) = states.get_mut(&symbol_id) {
                    state.initialized = true;
                }
            }
        } else {
            analyze_node(lhs, source, sema, states, order, active_stack, issues);
        }
    }
}

fn handle_identifier_use(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    issues: &mut Vec<DataflowIssue>,
) {
    if !is_identifier_read(node) {
        return;
    }

    let name = node_text(node, source).trim();
    if name.is_empty() {
        return;
    }

    let Some(symbol_id) = sema.resolve_symbol_at_node(node, name) else {
        return;
    };
    let Some(state) = states.get_mut(&symbol_id) else {
        return;
    };

    if !state.initialized {
        issues.push(DataflowIssue {
            code: "UECPP-DF-002",
            message: format!("Local variable {} may be used before it is initialized.", state.name),
            line: node.start_position().row as u32,
            character: node.start_position().column as u32,
            end_line: node.end_position().row as u32,
            end_character: node.end_position().column as u32,
        });
    }
    state.used = true;
}

fn walk_children(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        analyze_node(child, source, sema, states, order, active_stack, issues);
    }
}

fn analyze_declaration_initializer(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let mut cursor = declarator.walk();
    for child in declarator.children(&mut cursor) {
        if matches!(child.kind(), "identifier" | "field_identifier") {
            continue;
        }
        analyze_node(child, source, sema, states, order, active_stack, issues);
    }
}

fn collect_functions(node: Node, visit: &mut impl FnMut(Node)) {
    if matches!(node.kind(), "function_definition" | "unreal_function_definition") {
        visit(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, visit);
    }
}

fn find_descendant<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_descendant(child, kind) {
            return Some(found);
        }
    }
    None
}

fn find_name_node(node: Node) -> Option<Node> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node),
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "init_declarator"
        | "bitfield_clause" => node.child_by_field_name("declarator").and_then(find_name_node),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_name_node(child) {
                    return Some(found);
                }
            }
            None
        }
    }
}

fn declaration_has_initializer(node: Node, source: &str) -> bool {
    if node.kind() == "parameter_declaration" {
        return true;
    }
    find_descendant(node, "init_declarator").is_some() || node_text(node, source).contains('=')
}

fn is_identifier_read(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };

    if matches!(
        parent.kind(),
        "declaration"
            | "field_declaration"
            | "parameter_declaration"
            | "class_specifier"
            | "struct_specifier"
            | "enum_specifier"
            | "namespace_definition"
    ) {
        if parent
            .child_by_field_name("declarator")
            .and_then(find_name_node)
            .is_some_and(|name_node| name_node.byte_range() == node.byte_range())
        {
            return false;
        }
    }

    if parent.kind() == "assignment_expression"
        && parent
            .child_by_field_name("left")
            .or_else(|| parent.child(0))
            .is_some_and(|lhs| lhs.byte_range() == node.byte_range())
    {
        return false;
    }

    if parent.kind() == "field_expression"
        && parent
            .child_by_field_name("field")
            .is_some_and(|field| field.byte_range() == node.byte_range())
    {
        return false;
    }

    true
}

fn local_symbol_for_name(sema: &SemaContext, node: Node, name: &str) -> Option<SymbolId> {
    let scope_id = sema.scope_for_node(node);
    sema.lookup_name_at_node(node, name)
        .into_iter()
        .find(|symbol_id| {
            sema.symbols
                .get(*symbol_id)
                .is_some_and(|symbol| {
                    symbol.scope == scope_id
                        && matches!(
                            symbol.kind,
                            SymbolKind::Variable {
                                storage: Storage::Local | Storage::Parameter,
                                ..
                            }
                        )
                })
        })
}

fn lookup_active_name_in_parents(sema: &SemaContext, node: Node, name: &str) -> Option<SymbolId> {
    sema.lookup_name_in_parent_scopes(node, name)
        .into_iter()
        .find(|symbol_id| {
            sema.symbols
                .get(*symbol_id)
                .is_some_and(|symbol| matches!(symbol.kind, SymbolKind::Variable { .. }))
        })
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    let range = node.byte_range();
    if range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
    {
        &source[range.start..range.end]
    } else {
        ""
    }
}

fn is_raii_type(type_name: &str) -> bool {
    let prefixes = RAII_PREFIXES.get_or_init(|| {
        toml::from_str(include_str!("../../data/raii_type_prefixes.toml")).unwrap_or_default()
    });
    prefixes
        .prefixes
        .iter()
        .any(|prefix| type_name.starts_with(prefix))
}
