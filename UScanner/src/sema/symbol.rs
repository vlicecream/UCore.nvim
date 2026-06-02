use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::scope::ScopeId;
use super::types::TypeId;

pub type SymStr = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct SymbolId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ClassId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct EnumId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Storage {
    Local,
    Parameter,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Public,
    Protected,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuncOverload {
    pub type_id: TypeId,
    pub source: SourceRef,
    pub is_definition: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceRef {
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolKind {
    Class {
        class_id: ClassId,
        type_id: TypeId,
        parents: Vec<TypeId>,
        is_struct: bool,
    },
    Enum {
        enum_id: EnumId,
        type_id: TypeId,
    },
    Function {
        decls: Vec<FuncOverload>,
    },
    Variable {
        type_id: TypeId,
        storage: Storage,
    },
    Field {
        owner: ClassId,
        type_id: TypeId,
        access: Access,
    },
    Method {
        owner: ClassId,
        type_id: TypeId,
        access: Access,
        is_static: bool,
        is_virtual: bool,
        is_override: bool,
    },
    Typedef {
        type_id: TypeId,
    },
    Namespace {
        children: ScopeId,
    },
    Template,
    EnumValue {
        value: i64,
        enum_id: EnumId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: SymStr,
    pub scope: ScopeId,
    pub kind: SymbolKind,
}

pub struct SymbolTable {
    symbols: Vec<Symbol>,
    class_names: Vec<SymStr>,
    enum_names: Vec<SymStr>,
    class_ids_by_name: HashMap<SymStr, ClassId>,
    enum_ids_by_name: HashMap<SymStr, EnumId>,
    class_scopes: HashMap<ClassId, ScopeId>,
    class_parents: HashMap<ClassId, Vec<TypeId>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            class_names: Vec::new(),
            enum_names: Vec::new(),
            class_ids_by_name: HashMap::new(),
            enum_ids_by_name: HashMap::new(),
            class_scopes: HashMap::new(),
            class_parents: HashMap::new(),
        }
    }

    pub fn add_symbol(&mut self, name: impl Into<SymStr>, scope: ScopeId, kind: SymbolKind) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol {
            id,
            name: name.into(),
            scope,
            kind,
        });
        id
    }

    pub fn get(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(id.0 as usize)
    }

    pub fn new_class_id(&mut self, name: &str) -> ClassId {
        if let Some(id) = self.class_ids_by_name.get(name) {
            return *id;
        }

        let id = ClassId(self.class_names.len() as u32);
        self.class_names.push(name.to_string());
        self.class_ids_by_name.insert(name.to_string(), id);
        id
    }

    pub fn new_enum_id(&mut self, name: &str) -> EnumId {
        if let Some(id) = self.enum_ids_by_name.get(name) {
            return *id;
        }

        let id = EnumId(self.enum_names.len() as u32);
        self.enum_names.push(name.to_string());
        self.enum_ids_by_name.insert(name.to_string(), id);
        id
    }

    pub fn class_name(&self, id: ClassId) -> Option<&str> {
        self.class_names.get(id.0 as usize).map(String::as_str)
    }

    pub fn class_id_by_name(&self, name: &str) -> Option<ClassId> {
        self.class_ids_by_name.get(name).copied()
    }

    pub fn set_class_scope(&mut self, class_id: ClassId, scope_id: ScopeId) {
        self.class_scopes.insert(class_id, scope_id);
    }

    pub fn class_scope(&self, class_id: ClassId) -> Option<ScopeId> {
        self.class_scopes.get(&class_id).copied()
    }

    pub fn set_class_parents(&mut self, class_id: ClassId, parents: Vec<TypeId>) {
        self.class_parents.insert(class_id, parents);
    }

    pub fn class_parents(&self, class_id: ClassId) -> &[TypeId] {
        self.class_parents
            .get(&class_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn enum_name(&self, id: EnumId) -> Option<&str> {
        self.enum_names.get(id.0 as usize).map(String::as_str)
    }
}
