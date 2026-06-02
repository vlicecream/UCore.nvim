use tree_sitter::Node;

use super::lookup::lookup_name;
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

    let TypeKind::Class(class_id) = ctx.types.get(receiver_type)? else {
        return None;
    };
    let scope_id = ctx.symbols.class_scope(*class_id)?;
    for symbol_id in lookup_name(ctx, scope_id, field_name) {
        if let Some(type_id) = ctx.symbol_type(symbol_id) {
            return Some(type_id);
        }
    }

    None
}

fn type_of_call_expression(ctx: &SemaContext, node: Node) -> Option<TypeId> {
    let callee = node.child_by_field_name("function").or_else(|| node.child(0))?;
    let callee_type = type_of_expression(ctx, callee)?;
    let TypeKind::Function { return_t, .. } = ctx.types.get(callee_type)? else {
        return None;
    };
    Some(*return_t)
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
