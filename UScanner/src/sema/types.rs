use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::symbol::{ClassId, EnumId, SymStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuiltinType {
    Void,
    Bool,
    Char,
    Int32,
    UInt32,
    Float,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct CvQual {
    pub is_const: bool,
    pub is_volatile: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefKind {
    LValue,
    RValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TemplateArg {
    Type(TypeId),
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeKind {
    Builtin(BuiltinType),
    Pointer { pointee: TypeId, cv: CvQual },
    Reference { referent: TypeId, kind: RefKind },
    Array { elem: TypeId, size: Option<u64> },
    Function {
        return_t: TypeId,
        params: Vec<TypeId>,
        is_variadic: bool,
        is_const_member: bool,
    },
    Class(ClassId),
    Enum(EnumId),
    Typedef { name: SymStr, aliased: TypeId },
    Template { base: SymStr, args: Vec<TemplateArg> },
    Dependent(SymStr),
    Auto,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    Same,
    DerivedToBase,
    PointerToVoid,
    NumericPromote,
    NumericConvert,
    NullptrToPointer,
    Incompatible,
}

pub struct TypeArena {
    types: Vec<TypeKind>,
    interned: HashMap<TypeKind, TypeId>,
    pub void_t: TypeId,
    pub bool_t: TypeId,
    pub int32_t: TypeId,
    pub uint32_t: TypeId,
    pub float_t: TypeId,
    pub double_t: TypeId,
    pub unknown_t: TypeId,
}

impl TypeArena {
    pub fn new() -> Self {
        let mut arena = Self {
            types: Vec::new(),
            interned: HashMap::new(),
            void_t: TypeId(0),
            bool_t: TypeId(0),
            int32_t: TypeId(0),
            uint32_t: TypeId(0),
            float_t: TypeId(0),
            double_t: TypeId(0),
            unknown_t: TypeId(0),
        };

        arena.void_t = arena.intern(TypeKind::Builtin(BuiltinType::Void));
        arena.bool_t = arena.intern(TypeKind::Builtin(BuiltinType::Bool));
        arena.int32_t = arena.intern(TypeKind::Builtin(BuiltinType::Int32));
        arena.uint32_t = arena.intern(TypeKind::Builtin(BuiltinType::UInt32));
        arena.float_t = arena.intern(TypeKind::Builtin(BuiltinType::Float));
        arena.double_t = arena.intern(TypeKind::Builtin(BuiltinType::Double));
        arena.unknown_t = arena.intern(TypeKind::Unknown);
        arena
    }

    pub fn intern(&mut self, kind: TypeKind) -> TypeId {
        if let Some(existing) = self.interned.get(&kind) {
            return *existing;
        }

        let id = TypeId(self.types.len() as u32);
        self.types.push(kind.clone());
        self.interned.insert(kind, id);
        id
    }

    pub fn get(&self, id: TypeId) -> Option<&TypeKind> {
        self.types.get(id.0 as usize)
    }

    pub fn find(&self, kind: &TypeKind) -> Option<TypeId> {
        self.interned.get(kind).copied()
    }
}

pub fn compat_rank(value: Compat) -> u8 {
    match value {
        Compat::Same => 5,
        Compat::DerivedToBase => 4,
        Compat::PointerToVoid => 3,
        Compat::NumericPromote => 2,
        Compat::NumericConvert => 1,
        Compat::NullptrToPointer => 1,
        Compat::Incompatible => 0,
    }
}
