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
        "cast_expression" => cast_item(node, file_path, sema_ctx),
        "call_expression" => named_cast_item(node, file_path, sema_ctx),
        "binary_expression" => binary_item(node, file_path, sema_ctx, rules),
        "unary_expression" => unary_item(node, file_path, sema_ctx, rules),
        "pointer_expression" => unary_item(node, file_path, sema_ctx, rules),
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
    if is_dependent_type_at_node(sema_ctx, node, left_ty)
        || is_dependent_type_at_node(sema_ctx, node, right_ty)
    {
        return None;
    }
    let compat = contextual_compat(sema_ctx, right, right_ty, left_ty);
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

fn cast_item(node: Node, file_path: Option<&str>, sema_ctx: &SemaContext) -> Option<DiagnosticItem> {
    let type_node = node.child_by_field_name("type")?;
    let value_node = node.child_by_field_name("value")?;
    let source_ty = crate::sema::expr::type_of_expression(sema_ctx, value_node)?;
    let target_ty = sema_ctx.resolve_existing_type_node(type_node)?;
    if is_dependent_type_at_node(sema_ctx, node, source_ty)
        || is_dependent_type_at_node(sema_ctx, node, target_ty)
    {
        return None;
    }
    if sema_ctx.check_compat(source_ty, target_ty) != Compat::Incompatible {
        return None;
    }

    Some(diagnostic_for_range(
        file_path,
        node,
        DiagnosticSeverity::Warning,
        "UECPP-EXPR-005",
        format!(
            "Cast target is incompatible with the source type. From {} to {}.",
            render_type(sema_ctx, source_ty),
            render_type(sema_ctx, target_ty)
        ),
    ))
}

fn named_cast_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
) -> Option<DiagnosticItem> {
    let callee = node.child_by_field_name("function").or_else(|| node.child(0))?;
    if !crate::sema::expr::is_named_cast_callee(callee, sema_ctx) {
        return None;
    }
    let args = node.child_by_field_name("arguments").or_else(|| find_descendant(node, "argument_list"))?;
    let value_node = args.named_child(0)?;
    let source_ty = crate::sema::expr::type_of_expression(sema_ctx, value_node)?;
    let target_ty = crate::sema::expr::named_cast_target_type(sema_ctx, node)?;
    if is_dependent_type_at_node(sema_ctx, node, source_ty)
        || is_dependent_type_at_node(sema_ctx, node, target_ty)
    {
        return None;
    }
    if sema_ctx.check_compat(source_ty, target_ty) != Compat::Incompatible {
        return None;
    }

    Some(diagnostic_for_range(
        file_path,
        node,
        DiagnosticSeverity::Warning,
        "UECPP-EXPR-005",
        format!(
            "Cast target is incompatible with the source type. From {} to {}.",
            render_type(sema_ctx, source_ty),
            render_type(sema_ctx, target_ty)
        ),
    ))
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
    if is_dependent_type_at_node(sema_ctx, node, expr_ty)
        || is_dependent_type_at_node(sema_ctx, node, return_ty)
    {
        return None;
    }
    let compat = contextual_compat(sema_ctx, value, expr_ty, return_ty);
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
    if is_dependent_type_at_node(sema_ctx, node, left_ty)
        || is_dependent_type_at_node(sema_ctx, node, right_ty)
    {
        return None;
    }
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
    if is_dependent_type_at_node(sema_ctx, node, operand_ty) {
        return None;
    }
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
    if is_dependent_type_at_node(sema_ctx, node, receiver_ty) {
        return None;
    }
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

    let field = node.child_by_field_name("field")?;
    let field_name = node_text(field, sema_ctx)?.trim();
    if field_name.is_empty() {
        return None;
    }
    if !class_member_lookup_supported(sema_ctx, receiver_ty) {
        return None;
    }
    if sema_ctx
        .lookup_class_member_symbols(receiver_ty, field_name)
        .is_empty()
    {
        return Some(diagnostic_for_range(
            file_path,
            field,
            DiagnosticSeverity::Error,
            "UECPP-EXPR-004",
            format!(
                "Member {} does not exist on type {}.",
                field_name,
                render_type(sema_ctx, receiver_ty)
            ),
        ));
    }
    None
}

fn contextual_compat(
    sema_ctx: &SemaContext,
    value_node: Node,
    from_ty: TypeId,
    to_ty: TypeId,
) -> Compat {
    if from_ty == sema_ctx.types.unknown_t {
        if let Some(compat) = new_expression_pointer_compat(sema_ctx, value_node, to_ty) {
            return compat;
        }
    }
    sema_ctx.check_compat(from_ty, to_ty)
}

fn new_expression_pointer_compat(
    sema_ctx: &SemaContext,
    node: Node,
    target_ty: TypeId,
) -> Option<Compat> {
    if node.kind() != "new_expression" {
        return None;
    }
    let type_node = node.child_by_field_name("type")?;
    let pointee_ty = sema_ctx.resolve_existing_type_node(type_node)?;
    let TypeKind::Pointer {
        pointee: target_pointee,
        ..
    } = sema_ctx.types.get(target_ty)?
    else {
        return Some(Compat::Incompatible);
    };
    Some(sema_ctx.check_compat(pointee_ty, *target_pointee))
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
        canonical_type_kind(sema_ctx, type_id),
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
    matches!(canonical_type_kind(sema_ctx, type_id), Some(TypeKind::Pointer { .. }))
}

fn is_pointer_integer_mix(sema_ctx: &SemaContext, left_ty: TypeId, right_ty: TypeId) -> bool {
    (is_pointer_type(sema_ctx, left_ty) && is_numeric_type(sema_ctx, right_ty))
        || (is_pointer_type(sema_ctx, right_ty) && is_numeric_type(sema_ctx, left_ty))
}

fn class_member_lookup_supported(sema_ctx: &SemaContext, type_id: TypeId) -> bool {
    match sema_ctx.types.get(type_id) {
        Some(TypeKind::Class(_)) => true,
        Some(TypeKind::Pointer { pointee, .. }) => class_member_lookup_supported(sema_ctx, *pointee),
        Some(TypeKind::Reference { referent, .. }) => {
            class_member_lookup_supported(sema_ctx, *referent)
        }
        Some(TypeKind::Typedef { aliased, .. }) => class_member_lookup_supported(sema_ctx, *aliased),
        _ => false,
    }
}

fn canonical_type_kind(sema_ctx: &SemaContext, type_id: TypeId) -> Option<&TypeKind> {
    match sema_ctx.types.get(type_id)? {
        TypeKind::Typedef { aliased, .. } => canonical_type_kind(sema_ctx, *aliased),
        TypeKind::Reference { referent, .. } => canonical_type_kind(sema_ctx, *referent),
        kind => Some(kind),
    }
}

fn is_dependent_type_at_node(sema_ctx: &SemaContext, node: Node, type_id: TypeId) -> bool {
    let Some(template_params) = enclosing_template_param_names(node, sema_ctx) else {
        return false;
    };
    type_uses_template_param(sema_ctx, type_id, &template_params)
}

fn type_uses_template_param(
    sema_ctx: &SemaContext,
    type_id: TypeId,
    template_params: &[String],
) -> bool {
    let Some(kind) = sema_ctx.types.get(type_id) else {
        return false;
    };
    match kind {
        TypeKind::Class(class_id) => sema_ctx
            .symbols
            .class_name(*class_id)
            .is_some_and(|name| template_params.iter().any(|param| param == name)),
        TypeKind::Pointer { pointee, .. } => {
            type_uses_template_param(sema_ctx, *pointee, template_params)
        }
        TypeKind::Reference { referent, .. } => {
            type_uses_template_param(sema_ctx, *referent, template_params)
        }
        TypeKind::Array { elem, .. } => type_uses_template_param(sema_ctx, *elem, template_params),
        TypeKind::Typedef { aliased, .. } => {
            type_uses_template_param(sema_ctx, *aliased, template_params)
        }
        TypeKind::Template { base, args } => {
            template_params.iter().any(|param| param == base)
                || args.iter().any(|arg| match arg {
                    crate::sema::types::TemplateArg::Type(type_id) => {
                        type_uses_template_param(sema_ctx, *type_id, template_params)
                    }
                    crate::sema::types::TemplateArg::Value(_) => false,
                })
        }
        TypeKind::Dependent(name) => template_params.iter().any(|param| param == name),
        _ => false,
    }
}

fn enclosing_template_param_names(node: Node, sema_ctx: &SemaContext) -> Option<Vec<String>> {
    let source = sema_ctx.source()?;
    let mut current = Some(node);
    while let Some(cursor) = current {
        if cursor.kind() == "template_declaration" {
            let params = cursor.child_by_field_name("parameters")?;
            let mut names = Vec::new();
            let mut walk = params.walk();
            for child in params.children(&mut walk) {
                match child.kind() {
                    "type_parameter_declaration" | "optional_type_parameter_declaration" => {
                        if let Some(name) = find_descendant(child, "type_identifier")
                            .or_else(|| find_descendant(child, "identifier"))
                            .and_then(|ident| node_text(ident, sema_ctx))
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                        {
                            names.push(name.to_string());
                        }
                    }
                    "parameter_declaration" => {
                        if let Some(name) = child
                            .child_by_field_name("declarator")
                            .and_then(find_name_node)
                            .and_then(|ident| {
                                let range = ident.byte_range();
                                (range.end <= source.len()
                                    && source.is_char_boundary(range.start)
                                    && source.is_char_boundary(range.end))
                                    .then_some(&source[range.start..range.end])
                            })
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                        {
                            names.push(name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            return Some(names);
        }
        current = cursor.parent();
    }
    None
}

fn find_name_node(node: Node) -> Option<Node> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(node),
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "init_declarator"
        | "bitfield_clause" => next_declarator_node(node).and_then(find_name_node),
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

fn next_declarator_node(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "identifier"
                    | "field_identifier"
                    | "type_identifier"
                    | "qualified_identifier"
                    | "function_declarator"
                    | "pointer_declarator"
                    | "reference_declarator"
                    | "array_declarator"
                    | "parenthesized_declarator"
                    | "init_declarator"
                    | "bitfield_clause"
            )
        })
    })
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
