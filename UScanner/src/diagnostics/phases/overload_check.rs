use anyhow::Result;
use tree_sitter::Node;

use crate::diagnostics::{DiagnosticItem, DiagnosticSeverity, OverloadRules};
use crate::sema::SemaContext;
use crate::sema::overload::{self, CallResult};
use crate::sema::symbol::SymbolId;
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
    if is_template_callee(callee) {
        return None;
    }
    if let Some(template_index) = build_template_index_for_node(node, sema_ctx) {
        if template_index.analyze_call(node, sema_ctx).is_some()
            || template_index.infer_call_return_type(node, sema_ctx).is_some()
        {
            return None;
        }
    }
    let arg_types = argument_types(sema_ctx, node)?;
    let candidates = resolve_callee_symbols(sema_ctx, callee, &arg_types)?;
    if candidates.symbols.is_empty() {
        return None;
    }

    let result = if let Some(receiver_type) = candidates.receiver_type {
        let callable = candidates
            .symbols
            .iter()
            .filter_map(|symbol_id| {
                sema_ctx
                    .member_callable_signature(receiver_type, *symbol_id)
                    .map(|signature| (*symbol_id, signature))
            })
            .collect::<Vec<_>>();
        overload::resolve_call_with_signatures(sema_ctx, &callable, &arg_types)
    } else {
        overload::resolve_call_with_args(sema_ctx, &candidates.symbols, &arg_types)
    };

    match result {
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
            let arity_mismatch = if let Some(receiver_type) = candidates.receiver_type {
                candidates
                    .symbols
                    .iter()
                    .filter_map(|symbol_id| {
                        sema_ctx
                            .member_callable_signature(receiver_type, *symbol_id)
                            .map(signature_arity)
                    })
                    .all(|(min_arity, max_arity, is_variadic)| {
                        arg_count < min_arity || (!is_variadic && arg_count > max_arity)
                    })
            } else {
                candidates
                    .symbols
                    .iter()
                    .filter_map(|symbol_id| sema_ctx.symbol_type(*symbol_id))
                    .filter_map(|type_id| function_arity(sema_ctx, type_id))
                    .all(|(min_arity, max_arity, is_variadic)| {
                        arg_count < min_arity || (!is_variadic && arg_count > max_arity)
                    })
            };

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

fn is_template_callee(callee: Node) -> bool {
    match callee.kind() {
        "template_function" | "template_method" => true,
        "field_expression" => callee
            .child_by_field_name("field")
            .is_some_and(|field| matches!(field.kind(), "template_method" | "template_function")),
        _ => false,
    }
}

fn build_template_index_for_node(
    node: Node,
    ctx: &SemaContext,
) -> Option<crate::sema::template::TemplateIndex> {
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    Some(crate::sema::template::TemplateIndex::collect(root, ctx))
}

struct ResolvedCalleeSymbols {
    symbols: Vec<SymbolId>,
    receiver_type: Option<TypeId>,
}

fn resolve_callee_symbols(
    sema_ctx: &SemaContext,
    callee: Node,
    arg_types: &[TypeId],
) -> Option<ResolvedCalleeSymbols> {
    match callee.kind() {
        "identifier" | "field_identifier" => {
            let name = node_text(callee, sema_ctx)?.trim();
            (!name.is_empty()).then(|| ResolvedCalleeSymbols {
                symbols: sema_ctx.lookup_call_name_at_node(callee, name, arg_types),
                receiver_type: None,
            })
        }
        "qualified_identifier" => {
            let segments = qualified_identifier_segments(callee, sema_ctx)?;
            let refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
            (!refs.is_empty()).then(|| ResolvedCalleeSymbols {
                symbols: sema_ctx.lookup_qualified_name_at_node(callee, &refs),
                receiver_type: None,
            })
        }
        "field_expression" => {
            let receiver = callee.child_by_field_name("argument").or_else(|| callee.child(0))?;
            let field = callee.child_by_field_name("field")?;
            let receiver_type = crate::sema::expr::type_of_expression(sema_ctx, receiver)?;
            let field_name = node_text(field, sema_ctx)?.trim();
            (!field_name.is_empty()).then(|| ResolvedCalleeSymbols {
                symbols: sema_ctx.lookup_class_member_symbols(receiver_type, field_name),
                receiver_type: Some(receiver_type),
            })
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

fn function_arity(sema_ctx: &SemaContext, type_id: TypeId) -> Option<(usize, usize, bool)> {
    let TypeKind::Function {
        params,
        min_arity,
        is_variadic,
        ..
    } = sema_ctx.types.get(type_id)?
    else {
        return None;
    };
    Some((*min_arity, params.len(), *is_variadic))
}

fn signature_arity(signature: crate::sema::MemberCallableSignature) -> (usize, usize, bool) {
    (
        signature.min_arity,
        signature.params.len(),
        signature.is_variadic,
    )
}

fn qualified_identifier_segments(node: Node, sema_ctx: &SemaContext) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    collect_qualified_identifier_segments(node, sema_ctx, &mut segments)?;
    (!segments.is_empty()).then_some(segments)
}

fn collect_qualified_identifier_segments(
    node: Node,
    sema_ctx: &SemaContext,
    segments: &mut Vec<String>,
) -> Option<()> {
    if node.kind() != "qualified_identifier" {
        let text = node_text(node, sema_ctx)?.trim();
        if text.is_empty() {
            return Some(());
        }
        segments.extend(
            text.split("::")
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(ToOwned::to_owned),
        );
        return Some(());
    }

    if let Some(scope) = node.child_by_field_name("scope") {
        collect_qualified_identifier_segments(scope, sema_ctx, segments)?;
    }
    if let Some(name_node) = node.child_by_field_name("name") {
        if name_node.kind() == "qualified_identifier" {
            collect_qualified_identifier_segments(name_node, sema_ctx, segments)?;
            return Some(());
        }
        let name = node_text(name_node, sema_ctx)?.trim();
        if !name.is_empty() {
            segments.push(name.to_string());
        }
    }

    Some(())
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
