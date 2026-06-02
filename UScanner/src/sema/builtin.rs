use super::types::{BuiltinType, TypeArena, TypeId, TypeKind};

pub fn builtin_type_by_name(arena: &mut TypeArena, name: &str) -> Option<TypeId> {
    match name.trim() {
        "void" => Some(arena.void_t),
        "bool" => Some(arena.bool_t),
        "char" | "ANSICHAR" | "TCHAR" => Some(arena.intern(TypeKind::Builtin(BuiltinType::Char))),
        "int" | "int32" => Some(arena.int32_t),
        "uint32" => Some(arena.uint32_t),
        "float" => Some(arena.float_t),
        "double" => Some(arena.double_t),
        _ => None,
    }
}
