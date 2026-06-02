use anyhow::Result;
use tree_sitter::Node;

use crate::diagnostics::{DiagnosticItem, DiagnosticSeverity, OverloadRules};
use crate::sema::SemaContext;
use crate::sema::overload::{self, CallResult};
use crate::sema::types::{TypeId, TypeKind};

pub(crate) fn collect(
    file_path: Option<&str>,
    parsed_root: Option<Node>,
    sema_ctx: Option<&SemaContext>,
    rules: &OverloadRules,
) -> Result<Vec<DiagnosticItem>> {
    let Some(root) = parsed_root else {
        return Ok(Vec::new());
    };
    let Some(sema_ctx) = sema_ctx else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    collect_call_items(root, file_path, sema_ctx, rules, &mut items);
    Ok(items)
}

fn collect_call_items(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &OverloadRules,
    items: &mut Vec<DiagnosticItem>,
) {
    if node.kind() == "call_expression" {
        if let Some(item) = call_diagnostic_item(node, file_path, sema_ctx, rules) {
            items.push(item);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_items(child, file_path, sema_ctx, rules, items);
    }
}

fn call_diagnostic_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &OverloadRules,
) -> Option<DiagnosticItem> {
    let callee = node.child_by_field_name("function").or_else(|| node.child(0))?;
    let candidates = resolve_callee_symbols(sema_ctx, callee)?;
    if candidates.is_empty() {
        return None;
    }

    let arg_types = argument_types(sema_ctx, node)?;
    match overload::resolve_call_with_args(sema_ctx, &candidates, &arg_types) {
        CallResult::Ok(_) => None,
        CallResult::Ambiguous(_) => Some(
            diagnostic_for_range(
                file_path,
                callee,
                DiagnosticSeverity::from(rules.severity_ambiguous),
                "UECPP-EXPR-002",
                "Call is ambiguous across multiple overloads.",
            ),
        ),
        CallResult::NoMatch { .. } => {
            let arg_count = arg_types.len();
            let arity_mismatch = candidates
                .iter()
                .filter_map(|symbol_id| sema_ctx.symbol_type(*symbol_id))
                .filter_map(|type_id| function_arity(sema_ctx, type_id))
                .all(|(arity, is_variadic)| (!is_variadic && arity != arg_count) || (is_variadic && arg_count < arity));

            let (code, message) = if arity_mismatch {
                (
                    "UECPP-EXPR-003",
                    "Argument count does not match any overload.",
                )
            } else {
                (
                    "UECPP-EXPR-001",
                    "No matching overload for this call.",
                )
            };
            Some(diagnostic_for_range(
                file_path,
                callee,
                DiagnosticSeverity::from(rules.severity_no_match),
                code,
                message,
            ))
        }
    }
}

fn resolve_callee_symbols(sema_ctx: &SemaContext, callee: Node) -> Option<Vec<crate::sema::symbol::SymbolId>> {
    match callee.kind() {
        "identifier" | "field_identifier" => {
            let name = node_text(callee, sema_ctx)?.trim();
            (!name.is_empty()).then(|| sema_ctx.lookup_name_at_node(callee, name))
        }
        "qualified_identifier" => {
            let name_node = callee.child_by_field_name("name")?;
            let name = node_text(name_node, sema_ctx)?.trim();
            (!name.is_empty()).then(|| sema_ctx.lookup_name_at_node(callee, name))
        }
        "field_expression" => {
            let receiver = callee.child_by_field_name("argument").or_else(|| callee.child(0))?;
            let field = callee.child_by_field_name("field")?;
            let receiver_type = crate::sema::expr::type_of_expression(sema_ctx, receiver)?;
            let field_name = node_text(field, sema_ctx)?.trim();
            (!field_name.is_empty()).then(|| sema_ctx.lookup_class_member_symbols(receiver_type, field_name))
        }
        _ => None,
    }
}

fn argument_types(sema_ctx: &SemaContext, node: Node) -> Option<Vec<TypeId>> {
    let args = node.child_by_field_name("arguments").or_else(|| find_descendant(node, "argument_list"))?;
    let mut out = Vec::new();
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        out.push(crate::sema::expr::type_of_expression(sema_ctx, child)?);
    }
    Some(out)
}

fn function_arity(sema_ctx: &SemaContext, type_id: TypeId) -> Option<(usize, bool)> {
    let TypeKind::Function {
        params,
        is_variadic,
        ..
    } = sema_ctx.types.get(type_id)?
    else {
        return None;
    };
    Some((params.len(), *is_variadic))
}

fn diagnostic_for_range(
    file_path: Option<&str>,
    node: Node,
    severity: DiagnosticSeverity,
    code: &'static str,
    message: impl Into<String>,
) -> DiagnosticItem {
    DiagnosticItem::new(
        file_path,
        node.start_position().row as u32,
        node.start_position().column as u32,
        severity,
        "UCore",
        code,
        message,
    )
    .with_end(
        node.end_position().row as u32,
        node.end_position().column as u32,
    )
}

fn node_text<'a>(node: Node, sema_ctx: &'a SemaContext) -> Option<&'a str> {
    let range = node.byte_range();
    let source = sema_ctx.source()?;
    if range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
    {
        Some(&source[range.start..range.end])
    } else {
        None
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
