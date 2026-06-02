use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;
use tree_sitter::Node;

use super::cfg::{build_cfg, Cfg};
use super::symbol::{Storage, SymbolId, SymbolKind};
use super::types::TypeKind;
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
    let mut constant_values = HashMap::<SymbolId, i64>::new();
    let mut order = Vec::<SymbolId>::new();

    if let Some(body) = find_descendant(function_node, "compound_statement") {
        let mut active_stack = Vec::<HashSet<SymbolId>>::new();
        analyze_block(
            body,
            source,
            sema,
            &mut states,
            &mut constant_values,
            &mut order,
            &mut active_stack,
            &mut issues,
        );

        if function_requires_return_value(function_node, sema)
            && !statement_guarantees_return(body)
        {
            if let Some(name_node) = function_name_node(function_node) {
                let name = node_text(name_node, source).trim();
                issues.push(DataflowIssue {
                    code: "UECPP-DF-004",
                    message: format!(
                        "Non-void function {} can reach the end without returning a value.",
                        name
                    ),
                    line: name_node.start_position().row as u32,
                    character: name_node.start_position().column as u32,
                    end_line: name_node.end_position().row as u32,
                    end_character: name_node.end_position().column as u32,
                });
            }
        }
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
    constant_values: &mut HashMap<SymbolId, i64>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    active_stack.push(HashSet::new());

    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        analyze_node(
            child,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        );
    }

    active_stack.pop();
}

fn analyze_node(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    constant_values: &mut HashMap<SymbolId, i64>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    match node.kind() {
        "compound_statement" => analyze_block(
            node,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        ),
        "declaration" => {
            handle_declaration(
                node,
                source,
                sema,
                states,
                constant_values,
                order,
                active_stack,
                issues,
            );
            analyze_declaration_initializer(
                node,
                source,
                sema,
                states,
                constant_values,
                order,
                active_stack,
                issues,
            );
        }
        "parameter_declaration" => {
            handle_parameter(node, source, sema, states, active_stack, issues);
        }
        "assignment_expression" => {
            handle_assignment(
                node,
                source,
                sema,
                states,
                constant_values,
                order,
                active_stack,
                issues,
            );
        }
        "if_statement" | "while_statement" | "for_statement" | "do_statement" => {
            if let Some(condition) = control_condition_node(node) {
                if let Some(value) = evaluate_const_expr(condition, source, sema, constant_values) {
                    issues.push(DataflowIssue {
                        code: "UECPP-DF-005",
                        message: format!(
                            "Condition is always {}.",
                            if value == 0 { "false" } else { "true" }
                        ),
                        line: condition.start_position().row as u32,
                        character: condition.start_position().column as u32,
                        end_line: condition.end_position().row as u32,
                        end_character: condition.end_position().column as u32,
                    });
                }
            }
            walk_children(
                node,
                source,
                sema,
                states,
                constant_values,
                order,
                active_stack,
                issues,
            );
        }
        "identifier" => {
            handle_identifier_use(node, source, sema, states, issues);
        }
        _ => walk_children(
            node,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        ),
    }
}

fn handle_declaration(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    constant_values: &mut HashMap<SymbolId, i64>,
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
    let track_unused = !name.starts_with('_') && !is_raii_type(type_name.as_deref().unwrap_or_default());

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

    if is_const_like_declaration(node, source) {
        if let Some(initializer) = initializer_expression(node) {
            if let Some(value) = evaluate_const_expr(initializer, source, sema, constant_values) {
                constant_values.insert(symbol_id, value);
            }
        }
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
    constant_values: &mut HashMap<SymbolId, i64>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    let lhs = node.child_by_field_name("left").or_else(|| node.child(0));
    let rhs = node.child_by_field_name("right").or_else(|| node.child(2));

    if let Some(rhs) = rhs {
        analyze_node(
            rhs,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        );
    }

    if let Some(lhs) = lhs {
        if lhs.kind() == "identifier" {
            let name = node_text(lhs, source).trim();
            if let Some(symbol_id) = sema.resolve_symbol_at_node(lhs, name) {
                if let Some(state) = states.get_mut(&symbol_id) {
                    state.initialized = true;
                }
                constant_values.remove(&symbol_id);
            }
        } else {
            analyze_node(
                lhs,
                source,
                sema,
                states,
                constant_values,
                order,
                active_stack,
                issues,
            );
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
    constant_values: &mut HashMap<SymbolId, i64>,
    order: &mut Vec<SymbolId>,
    active_stack: &mut Vec<HashSet<SymbolId>>,
    issues: &mut Vec<DataflowIssue>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        analyze_node(
            child,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        );
    }
}

fn analyze_declaration_initializer(
    node: Node,
    source: &str,
    sema: &SemaContext,
    states: &mut HashMap<SymbolId, LocalVarState>,
    constant_values: &mut HashMap<SymbolId, i64>,
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
        analyze_node(
            child,
            source,
            sema,
            states,
            constant_values,
            order,
            active_stack,
            issues,
        );
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

fn function_requires_return_value(function_node: Node, sema: &SemaContext) -> bool {
    if node_text(function_node, sema.source().unwrap_or_default()).contains("[[noreturn]]") {
        return false;
    }

    let Some(return_type) = sema.enclosing_function_return_type(function_node) else {
        return false;
    };
    !matches!(
        sema.types.get(return_type),
        Some(TypeKind::Builtin(super::types::BuiltinType::Void))
            | Some(TypeKind::Auto)
            | Some(TypeKind::Unknown)
    )
}

fn statement_guarantees_return(node: Node) -> bool {
    match node.kind() {
        "return_statement" | "co_return_statement" | "throw_statement" => true,
        "compound_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if statement_guarantees_return(child) {
                    return true;
                }
            }
            false
        }
        "else_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if statement_guarantees_return(child) {
                    return true;
                }
            }
            false
        }
        "if_statement" => {
            let Some(consequence) = node.child_by_field_name("consequence") else {
                return false;
            };
            let Some(alternative) = node.child_by_field_name("alternative") else {
                return false;
            };
            statement_guarantees_return(consequence) && statement_guarantees_return(alternative)
        }
        _ => false,
    }
}

fn control_condition_node(node: Node) -> Option<Node> {
    node.child_by_field_name("condition").or_else(|| {
        let mut cursor = node.walk();
        node.children(&mut cursor).find(|child| child.kind() == "condition_clause")
    }).and_then(|condition| {
        if condition.kind() == "condition_clause" {
            condition.named_child(0)
        } else {
            Some(condition)
        }
    })
}

fn evaluate_const_expr(
    node: Node,
    source: &str,
    sema: &SemaContext,
    constant_values: &HashMap<SymbolId, i64>,
) -> Option<i64> {
    match node.kind() {
        "true" => Some(1),
        "false" | "null" => Some(0),
        "number_literal" => parse_numeric_literal(node_text(node, source)),
        "identifier" | "field_identifier" => {
            let name = node_text(node, source).trim();
            let symbol_id = sema.resolve_symbol_at_node(node, name)?;
            constant_values.get(&symbol_id).copied()
        }
        "parenthesized_expression" => node.named_child(0).and_then(|inner| {
            evaluate_const_expr(inner, source, sema, constant_values)
        }),
        "unary_expression" => {
            let operand = node
                .child_by_field_name("argument")
                .or_else(|| {
                    let index = node.named_child_count().saturating_sub(1);
                    u32::try_from(index).ok().and_then(|value| node.named_child(value))
                })?;
            let value = evaluate_const_expr(operand, source, sema, constant_values)?;
            let text = node_text(node, source).trim_start();
            if text.starts_with('!') {
                Some(i64::from(value == 0))
            } else if text.starts_with('-') {
                Some(-value)
            } else if text.starts_with('+') {
                Some(value)
            } else {
                None
            }
        }
        "binary_expression" => {
            let left = node.child_by_field_name("left").or_else(|| node.child(0))?;
            let right = node.child_by_field_name("right").or_else(|| node.child(2))?;
            let left_value = evaluate_const_expr(left, source, sema, constant_values)?;
            let right_value = evaluate_const_expr(right, source, sema, constant_values)?;
            match operator_text(node, source)?.as_str() {
                "||" => Some(i64::from(left_value != 0 || right_value != 0)),
                "&&" => Some(i64::from(left_value != 0 && right_value != 0)),
                "==" => Some(i64::from(left_value == right_value)),
                "!=" => Some(i64::from(left_value != right_value)),
                "<" => Some(i64::from(left_value < right_value)),
                "<=" => Some(i64::from(left_value <= right_value)),
                ">" => Some(i64::from(left_value > right_value)),
                ">=" => Some(i64::from(left_value >= right_value)),
                "+" => Some(left_value + right_value),
                "-" => Some(left_value - right_value),
                "*" => Some(left_value * right_value),
                "/" => (right_value != 0).then_some(left_value / right_value),
                "%" => (right_value != 0).then_some(left_value % right_value),
                _ => None,
            }
        }
        "conditional_expression" => {
            let condition = node.child_by_field_name("condition")?;
            let consequence = node.child_by_field_name("consequence")?;
            let alternative = node.child_by_field_name("alternative")?;
            let value = evaluate_const_expr(condition, source, sema, constant_values)?;
            if value != 0 {
                evaluate_const_expr(consequence, source, sema, constant_values)
            } else {
                evaluate_const_expr(alternative, source, sema, constant_values)
            }
        }
        _ => None,
    }
}

fn parse_numeric_literal(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let unsigned = trimmed.trim_end_matches(|ch: char| ch.is_ascii_alphabetic());
    if unsigned.starts_with("0x") || unsigned.starts_with("0X") {
        i64::from_str_radix(
            unsigned.trim_start_matches("0x").trim_start_matches("0X"),
            16,
        )
        .ok()
    } else if let Ok(value) = unsigned.parse::<i64>() {
        Some(value)
    } else if let Ok(value) = unsigned.parse::<f64>() {
        Some(i64::from(value != 0.0))
    } else {
        None
    }
}

fn operator_text(node: Node, source: &str) -> Option<String> {
    if let Some(operator) = node.child_by_field_name("operator") {
        return Some(node_text(operator, source).to_string());
    }

    let text = node_text(node, source);
    for operator in ["||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%"] {
        if text.contains(operator) {
            return Some(operator.to_string());
        }
    }
    None
}

fn initializer_expression(node: Node) -> Option<Node> {
    let declarator = node.child_by_field_name("declarator")?;
    find_initializer_expression(declarator)
}

fn find_initializer_expression(node: Node) -> Option<Node> {
    if node.kind() == "init_declarator" {
        let inner_declarator = node.child_by_field_name("declarator");
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if inner_declarator.is_some_and(|decl| decl.byte_range() == child.byte_range()) {
                continue;
            }
            return Some(child);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_initializer_expression(child) {
            return Some(found);
        }
    }
    None
}

fn is_const_like_declaration(node: Node, source: &str) -> bool {
    let text = node_text(node, source);
    text.contains("constexpr ") || text.contains(" const ") || text.starts_with("const ")
}

fn function_name_node(node: Node) -> Option<Node> {
    let declarator = node
        .child_by_field_name("declarator")
        .or_else(|| find_descendant(node, "function_declarator"))?;
    find_name_node(declarator)
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
