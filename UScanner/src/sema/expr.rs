use tree_sitter::Node;

use super::overload::resolve_call_with_args;
use super::types::{BuiltinType, CvQual, TypeId, TypeKind};
use super::SemaContext;

pub fn type_of_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    match node.kind() {
        "number_literal" => {
            let text = node_text(node, ctx)?;
            if text.contains('.') || text.ends_with('f') || text.ends_with('F') {
                Some(ctx.types.float_t)
            } else {
                Some(ctx.types.int32_t)
            }
        }
        "null" => Some(ctx.types.unknown_t),
        "string_literal" => Some(ctx.types_pointer_to_char()),
        "char_literal" => Some(ctx.types_char()),
        "true" | "false" => Some(ctx.types.bool_t),
        "identifier" | "field_identifier" => {
            let name = node_text(node, ctx)?.trim();
            if name.is_empty() {
                return None;
            }
            ctx.type_of_identifier_at_node(node, name)
        }
        "qualified_identifier" => {
            let name_node = node.child_by_field_name("name")?;
            let name = node_text(name_node, ctx)?.trim();
            if name.is_empty() {
                return None;
            }
            ctx.type_of_identifier_at_node(node, name)
        }
        "field_expression" => type_of_field_expression(ctx, node),
        "parenthesized_expression" => {
            let inner = node.named_child(0)?;
            type_of_expression(ctx, inner)
        }
        "assignment_expression" => {
            let lhs = node.child_by_field_name("left").or_else(|| node.child(0))?;
            type_of_expression(ctx, lhs)
        }
        "binary_expression" => type_of_binary_expression(ctx, node),
        "unary_expression" => type_of_unary_expression(ctx, node),
        "call_expression" => type_of_call_expression(ctx, node),
        _ => None,
    }
}

fn type_of_field_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    let receiver = node.child_by_field_name("argument").or_else(|| node.child(0))?;
    let field = node.child_by_field_name("field")?;
    let receiver_type = type_of_expression(ctx, receiver)?;
    let field_name = node_text(field, ctx)?.trim();
    if field_name.is_empty() {
        return None;
    }

    for symbol_id in ctx.lookup_class_member_symbols(receiver_type, field_name) {
        if let Some(type_id) = ctx.symbol_type(symbol_id) {
            return Some(type_id);
        }
    }

    None
}

fn type_of_call_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    let callee = node.child_by_field_name("function").or_else(|| node.child(0))?;
    let template_index = build_template_index_for_node(node, ctx)?;
    if is_template_callee(callee) {
        return template_index.infer_call_return_type(node, ctx);
    }
    if let Some(type_id) = template_index.infer_call_return_type(node, ctx) {
        return Some(type_id);
    }
    let arg_types = call_argument_types(ctx, node);
    match callee.kind() {
        "identifier" | "qualified_identifier" => {
            let callee_name = match callee.kind() {
                "identifier" => node_text(callee, ctx)?.trim().to_string(),
                _ => node_text(callee.child_by_field_name("name")?, ctx)?.trim().to_string(),
            };
            let symbols = ctx.lookup_name_at_node(callee, &callee_name);
            match resolve_call_with_args(ctx, &symbols, &arg_types) {
                super::overload::CallResult::Ok(symbol_id) => {
                    let fn_type = ctx.symbol_type(symbol_id)?;
                    let TypeKind::Function { return_t, .. } = ctx.types.get(fn_type)? else {
                        return Some(fn_type);
                    };
                    Some(*return_t)
                }
                _ => None,
            }
        }
        "field_expression" => {
            let field = callee.child_by_field_name("field")?;
            let receiver = callee.child_by_field_name("argument").or_else(|| callee.child(0))?;
            let receiver_type = type_of_expression(ctx, receiver)?;
            let field_name = node_text(field, ctx)?.trim();
            let symbols = ctx.lookup_class_member_symbols(receiver_type, field_name);
            match resolve_call_with_args(ctx, &symbols, &arg_types) {
                super::overload::CallResult::Ok(symbol_id) => {
                    let fn_type = ctx.symbol_type(symbol_id)?;
                    let TypeKind::Function { return_t, .. } = ctx.types.get(fn_type)? else {
                        return Some(fn_type);
                    };
                    Some(*return_t)
                }
                _ => None,
            }
        }
        _ => {
            let callee_type = type_of_expression(ctx, callee)?;
            let TypeKind::Function { return_t, .. } = ctx.types.get(callee_type)? else {
                return None;
            };
            Some(*return_t)
        }
    }
}

fn is_template_callee(callee: Node) -> bool {
    match callee.kind() {
        "template_function" | "template_method" => true,
        "field_expression" => callee
            .child_by_field_name("field")
            .is_some_and(|field| matches!(field.kind(), "template_function" | "template_method")),
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

fn type_of_binary_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    let left = node.child_by_field_name("left").or_else(|| node.child(0))?;
    let right = node.child_by_field_name("right").or_else(|| node.child(2))?;
    let left_ty = type_of_expression(ctx, left)?;
    let right_ty = type_of_expression(ctx, right)?;
    let text = node_text(node, ctx)?;

    if text.contains("==")
        || text.contains("!=")
        || text.contains('<')
        || text.contains('>')
    {
        return Some(ctx.types.bool_t);
    }

    let left_kind = ctx.types.get(left_ty)?;
    let right_kind = ctx.types.get(right_ty)?;
    match (left_kind, right_kind) {
        (TypeKind::Builtin(BuiltinType::Double), _) | (_, TypeKind::Builtin(BuiltinType::Double)) => {
            Some(ctx.types.double_t)
        }
        (TypeKind::Builtin(BuiltinType::Float), _) | (_, TypeKind::Builtin(BuiltinType::Float)) => {
            Some(ctx.types.float_t)
        }
        (TypeKind::Builtin(BuiltinType::Int32), _) | (_, TypeKind::Builtin(BuiltinType::Int32)) => {
            Some(ctx.types.int32_t)
        }
        _ => Some(left_ty),
    }
}

fn type_of_unary_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    let operand = node.child_by_field_name("argument").or_else(|| node.child(1)).or_else(|| node.named_child(0))?;
    let text = node_text(node, ctx)?;
    let operand_ty = type_of_expression(ctx, operand)?;

    if text.starts_with('!') {
        return Some(ctx.types.bool_t);
    }
    if text.starts_with('*') {
        if let Some(TypeKind::Pointer { pointee, .. }) = ctx.types.get(operand_ty) {
            return Some(*pointee);
        }
    }

    Some(operand_ty)
}

fn call_argument_types(ctx: &SemaContext, node: Node) -> Vec<TypeId> {
    let mut out = Vec::new();
    if let Some(args) = find_descendant(node, "argument_list") {
        let mut cursor = args.walk();
        for child in args.children(&mut cursor) {
            if child.is_named() {
                if let Some(type_id) = type_of_expression(ctx, child) {
                    out.push(type_id);
                }
            }
        }
    }
    out
}

fn node_text<'a>(node: Node, ctx: &'a SemaContext) -> Option<&'a str> {
    let range = node.byte_range();
    let source = ctx.source()?;
    if range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
    {
        Some(&source[range.start..range.end])
    } else {
        None
    }
}

pub fn attach_builtin_helpers(ctx: &mut SemaContext) {
    let char_t = ctx
        .types
        .intern(TypeKind::Builtin(BuiltinType::Char));
    let char_ptr = ctx.types.intern(TypeKind::Pointer {
        pointee: char_t,
        cv: CvQual::default(),
    });
    ctx.cached_char_t = Some(char_t);
    ctx.cached_char_ptr_t = Some(char_ptr);
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
