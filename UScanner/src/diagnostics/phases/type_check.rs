use anyhow::Result;
use tree_sitter::Node;

use crate::diagnostics::{DiagnosticItem, DiagnosticSeverity, TypeCheckRules};
use crate::sema::SemaContext;
use crate::sema::types::{BuiltinType, Compat, TypeId, TypeKind};

pub(crate) fn collect(
    file_path: Option<&str>,
    parsed_root: Option<Node>,
    sema_ctx: Option<&SemaContext>,
    rules: &TypeCheckRules,
) -> Result<Vec<DiagnosticItem>> {
    let Some(root) = parsed_root else {
        return Ok(Vec::new());
    };
    let Some(sema_ctx) = sema_ctx else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    collect_type_items(root, file_path, sema_ctx, rules, &mut items);
    Ok(items)
}

fn collect_type_items(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TypeCheckRules,
    items: &mut Vec<DiagnosticItem>,
) {
    if let Some(item) = type_diagnostic_item(node, file_path, sema_ctx, rules) {
        items.push(item);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_items(child, file_path, sema_ctx, rules, items);
    }
}

fn type_diagnostic_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TypeCheckRules,
) -> Option<DiagnosticItem> {
    match node.kind() {
        "assignment_expression" => assignment_item(node, file_path, sema_ctx, rules),
        "return_statement" => return_item(node, file_path, sema_ctx, rules),
        "binary_expression" => binary_item(node, file_path, sema_ctx, rules),
        "unary_expression" => unary_item(node, file_path, sema_ctx, rules),
        "field_expression" => field_item(node, file_path, sema_ctx),
        _ => None,
    }
}

fn assignment_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TypeCheckRules,
) -> Option<DiagnosticItem> {
    let left = node.child_by_field_name("left").or_else(|| node.child(0))?;
    let right = node.child_by_field_name("right").or_else(|| node.child(2))?;
    let left_ty = crate::sema::expr::type_of_expression(sema_ctx, left)?;
    let right_ty = crate::sema::expr::type_of_expression(sema_ctx, right)?;
    let compat = sema_ctx.check_compat(right_ty, left_ty);
    build_compat_item(
        file_path,
        node,
        sema_ctx,
        compat,
        right_ty,
        left_ty,
        rules,
        "UECPP-EXPR-008",
        "Assignment type is incompatible with the destination type.",
        "Assignment performs a narrowing conversion.",
    )
}

fn return_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TypeCheckRules,
) -> Option<DiagnosticItem> {
    let value = node.named_child(0)?;
    let expr_ty = crate::sema::expr::type_of_expression(sema_ctx, value)?;
    let return_ty = sema_ctx.enclosing_function_return_type(node)?;
    let compat = sema_ctx.check_compat(expr_ty, return_ty);
    build_compat_item(
        file_path,
        value,
        sema_ctx,
        compat,
        expr_ty,
        return_ty,
        rules,
        "UECPP-EXPR-007",
        "Return value is incompatible with the declared return type.",
        "Return value performs a narrowing conversion.",
    )
}

fn binary_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    _rules: &TypeCheckRules,
) -> Option<DiagnosticItem> {
    let left = node.child_by_field_name("left").or_else(|| node.child(0))?;
    let right = node.child_by_field_name("right").or_else(|| node.child(2))?;
    let left_ty = crate::sema::expr::type_of_expression(sema_ctx, left)?;
    let right_ty = crate::sema::expr::type_of_expression(sema_ctx, right)?;
    let operator = operator_text(node, sema_ctx)?;

    if matches!(operator, "==" | "!=" | "<" | "<=" | ">" | ">=") {
        if is_pointer_integer_mix(sema_ctx, left_ty, right_ty) {
            return Some(diagnostic_for_range(
                file_path,
                node,
                DiagnosticSeverity::Warning,
                "UECPP-EXPR-009",
                "Comparison mixes pointer and integer types.",
            ));
        }
        return None;
    }

    if matches!(operator, "+" | "-" | "*" | "/" | "%")
        && !(is_numeric_type(sema_ctx, left_ty) && is_numeric_type(sema_ctx, right_ty))
    {
        return Some(diagnostic_for_range(
            file_path,
            node,
            DiagnosticSeverity::Error,
            "UECPP-EXPR-006",
            "Binary operator is applied to incompatible operand types.",
        ));
    }

    None
}

fn unary_item(node: Node, file_path: Option<&str>, sema_ctx: &SemaContext, _rules: &TypeCheckRules) -> Option<DiagnosticItem> {
    let operand = node
        .child_by_field_name("argument")
        .or_else(|| node.child(1))
        .or_else(|| node.named_child(0))?;
    let operand_ty = crate::sema::expr::type_of_expression(sema_ctx, operand)?;
    let text = node_text(node, sema_ctx)?;

    if text.starts_with('*') && !is_pointer_type(sema_ctx, operand_ty) {
        return Some(diagnostic_for_range(
            file_path,
            node,
            DiagnosticSeverity::Error,
            "UECPP-EXPR-010",
            "Cannot dereference a non-pointer expression.",
        ));
    }

    None
}

fn field_item(node: Node, file_path: Option<&str>, sema_ctx: &SemaContext) -> Option<DiagnosticItem> {
    let receiver = node.child_by_field_name("argument").or_else(|| node.child(0))?;
    let receiver_ty = crate::sema::expr::type_of_expression(sema_ctx, receiver)?;
    let operator = operator_text(node, sema_ctx)?;
    if operator == "->" && !is_pointer_type(sema_ctx, receiver_ty) {
        return Some(diagnostic_for_range(
            file_path,
            node,
            DiagnosticSeverity::Error,
            "UECPP-EXPR-011",
            "Operator '->' requires a pointer receiver.",
        ));
    }
    None
}

fn build_compat_item(
    file_path: Option<&str>,
    node: Node,
    sema_ctx: &SemaContext,
    compat: Compat,
    from_ty: TypeId,
    to_ty: TypeId,
    rules: &TypeCheckRules,
    code: &'static str,
    incompatible_message: &'static str,
    narrowing_message: &'static str,
) -> Option<DiagnosticItem> {
    match compat {
        Compat::Incompatible => Some(diagnostic_for_range(
            file_path,
            node,
            DiagnosticSeverity::from(rules.severity_incompatible),
            code,
            format!(
                "{} From {} to {}.",
                incompatible_message,
                render_type(sema_ctx, from_ty),
                render_type(sema_ctx, to_ty)
            ),
        )),
        Compat::NumericConvert => Some(diagnostic_for_range(
            file_path,
            node,
            DiagnosticSeverity::from(rules.severity_narrowing),
            code,
            format!(
                "{} From {} to {}.",
                narrowing_message,
                render_type(sema_ctx, from_ty),
                render_type(sema_ctx, to_ty)
            ),
        )),
        _ => None,
    }
}

fn render_type(sema_ctx: &SemaContext, type_id: TypeId) -> String {
    sema_ctx
        .render_type(type_id)
        .unwrap_or_else(|| "unknown".to_string())
}

fn is_numeric_type(sema_ctx: &SemaContext, type_id: TypeId) -> bool {
    matches!(
        sema_ctx.types.get(type_id),
        Some(TypeKind::Builtin(
            BuiltinType::Char
                | BuiltinType::Int32
                | BuiltinType::UInt32
                | BuiltinType::Float
                | BuiltinType::Double
        ))
    )
}

fn is_pointer_type(sema_ctx: &SemaContext, type_id: TypeId) -> bool {
    matches!(sema_ctx.types.get(type_id), Some(TypeKind::Pointer { .. }))
}

fn is_pointer_integer_mix(sema_ctx: &SemaContext, left_ty: TypeId, right_ty: TypeId) -> bool {
    (is_pointer_type(sema_ctx, left_ty) && is_numeric_type(sema_ctx, right_ty))
        || (is_pointer_type(sema_ctx, right_ty) && is_numeric_type(sema_ctx, left_ty))
}

fn operator_text<'a>(node: Node, sema_ctx: &'a SemaContext) -> Option<&'a str> {
    node.child_by_field_name("operator")
        .and_then(|operator| node_text(operator, sema_ctx))
        .or_else(|| {
            let text = node_text(node, sema_ctx)?;
            ["==", "!=", "<=", ">=", "->", "+", "-", "*", "/", "%", "<", ">", "="]
                .into_iter()
                .find(|operator| text.contains(operator))
        })
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
