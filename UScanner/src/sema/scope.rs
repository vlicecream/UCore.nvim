use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::symbol::{SymbolId, SymStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ScopeId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScopeKind {
    Global,
    Namespace,
    Class,
    Function,
    Block,
    Template,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub symbols: HashMap<SymStr, Vec<SymbolId>>,
    pub using_decls: Vec<SymStr>,
    pub anon_count: u32,
}

pub struct ScopeTree {
    scopes: Vec<Scope>,
    pub global: ScopeId,
}

impl ScopeTree {
    pub fn new() -> Self {
        let global = ScopeId(0);
        Self {
            scopes: vec![Scope {
                parent: None,
                kind: ScopeKind::Global,
                symbols: HashMap::new(),
                using_decls: Vec::new(),
                anon_count: 0,
            }],
            global,
        }
    }

    pub fn add_scope(&mut self, parent: Option<ScopeId>, kind: ScopeKind) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            parent,
            kind,
            symbols: HashMap::new(),
            using_decls: Vec::new(),
            anon_count: 0,
        });
        id
    }

    pub fn get(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.0 as usize)
    }

    pub fn get_mut(&mut self, id: ScopeId) -> Option<&mut Scope> {
        self.scopes.get_mut(id.0 as usize)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.scopes.iter()
    }
}
