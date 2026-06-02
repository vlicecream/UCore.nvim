pub mod builtin;
pub mod builder;
pub mod cfg;
pub mod dataflow;
pub mod expr;
pub mod lookup;
pub mod overload;
pub mod scope;
pub mod symbol;
pub mod types;

use std::collections::HashMap;

use tree_sitter::Node;

use scope::{ScopeId, ScopeTree};
use symbol::{Access, Storage, SymbolId, SymbolKind, SymbolTable};
use types::{BuiltinType, TypeArena, TypeId, TypeKind};

pub struct SemaContext {
    pub types: TypeArena,
    pub symbols: SymbolTable,
    pub scopes: ScopeTree,
    pub node_scopes: HashMap<(usize, usize), ScopeId>,
    source: Option<String>,
    pub(crate) cached_char_t: Option<TypeId>,
    pub(crate) cached_char_ptr_t: Option<TypeId>,
}

#[cfg(test)]
mod tests {
    use super::{builder::build_sema, expr::type_of_expression};
    use tree_sitter::Parser;

    fn parse_root(content: &str) -> tree_sitter::Node<'_> {
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(content, None).unwrap();
        Box::leak(Box::new(tree)).root_node()
    }

    fn find_identifier<'a>(
        node: tree_sitter::Node<'a>,
        name: &str,
        content: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(child.kind(), "identifier" | "field_identifier" | "type_identifier") {
                let range = child.byte_range();
                if range.end <= content.len() && &content[range.start..range.end] == name {
                    return Some(child);
                }
            }
            if let Some(found) = find_identifier(child, name, content) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn sema_lookup_resolves_local_variable() {
        let content = "void Run() { int32 Value = 1; Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_identifier(root, "Value", content).unwrap();
        assert!(sema.symbol_exists_at_node(node, "Value"));
    }

    #[test]
    fn sema_expr_resolves_field_type() {
        let content = "class UFoo { public: int32 Count; void Run() { Count; } };";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_identifier(root, "Count", content).unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }
}

impl SemaContext {
    pub fn scope_for_node(&self, node: Node) -> ScopeId {
        let mut current = Some(node);
        while let Some(node) = current {
            if let Some(scope_id) = self.node_scopes.get(&(node.start_byte(), node.end_byte())) {
                return *scope_id;
            }
            current = node.parent();
        }
        self.scopes.global
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn lookup_name_at_node(&self, node: Node, name: &str) -> Vec<SymbolId> {
        let scope = self.scope_for_node(node);
        lookup::lookup_name(self, scope, name)
    }

    pub fn lookup_name_in_parent_scopes(&self, node: Node, name: &str) -> Vec<SymbolId> {
        let mut current = self.scopes.get(self.scope_for_node(node)).and_then(|scope| scope.parent);
        while let Some(scope_id) = current {
            let Some(scope) = self.scopes.get(scope_id) else {
                break;
            };
            if let Some(ids) = scope.symbols.get(name) {
                return ids.clone();
            }
            current = scope.parent;
        }
        Vec::new()
    }

    pub fn resolve_symbol_at_node(&self, node: Node, name: &str) -> Option<SymbolId> {
        self.lookup_name_at_node(node, name).into_iter().next()
    }

    pub fn type_of_identifier_at_node(&self, node: Node, name: &str) -> Option<TypeId> {
        self.lookup_name_at_node(node, name)
            .into_iter()
            .find_map(|symbol_id| self.symbol_type(symbol_id))
    }

    pub fn symbol_type(&self, symbol_id: SymbolId) -> Option<TypeId> {
        let symbol = self.symbols.get(symbol_id)?;
        match &symbol.kind {
            SymbolKind::Class { type_id, .. }
            | SymbolKind::Enum { type_id, .. }
            | SymbolKind::Variable { type_id, .. }
            | SymbolKind::Field { type_id, .. }
            | SymbolKind::Method { type_id, .. }
            | SymbolKind::Typedef { type_id } => Some(*type_id),
            SymbolKind::Function { decls } => decls.first().map(|decl| decl.type_id),
            _ => None,
        }
    }

    pub fn render_type(&self, type_id: TypeId) -> Option<String> {
        let ty = self.types.get(type_id)?;
        Some(match ty {
            TypeKind::Builtin(kind) => match kind {
                BuiltinType::Void => "void".to_string(),
                BuiltinType::Bool => "bool".to_string(),
                BuiltinType::Char => "char".to_string(),
                BuiltinType::Int32 => "int32".to_string(),
                BuiltinType::UInt32 => "uint32".to_string(),
                BuiltinType::Float => "float".to_string(),
                BuiltinType::Double => "double".to_string(),
            },
            TypeKind::Pointer { pointee, .. } => {
                format!("{}*", self.render_type(*pointee).unwrap_or_else(|| "unknown".to_string()))
            }
            TypeKind::Reference { referent, .. } => {
                format!("{}&", self.render_type(*referent).unwrap_or_else(|| "unknown".to_string()))
            }
            TypeKind::Array { elem, .. } => {
                format!("{}[]", self.render_type(*elem).unwrap_or_else(|| "unknown".to_string()))
            }
            TypeKind::Function { return_t, .. } => {
                self.render_type(*return_t).unwrap_or_else(|| "unknown".to_string())
            }
            TypeKind::Class(class_id) => self
                .symbols
                .class_name(*class_id)
                .unwrap_or("unknown")
                .to_string(),
            TypeKind::Enum(enum_id) => self
                .symbols
                .enum_name(*enum_id)
                .unwrap_or("unknown")
                .to_string(),
            TypeKind::Typedef { name, .. } => name.clone(),
            TypeKind::Template { base, .. } => base.clone(),
            TypeKind::Dependent(name) => name.clone(),
            TypeKind::Auto => "auto".to_string(),
            TypeKind::Unknown => "unknown".to_string(),
        })
    }

    pub fn symbol_exists_at_node(&self, node: Node, name: &str) -> bool {
        self.lookup_name_at_node(node, name).into_iter().any(|symbol_id| {
            self.symbols
                .get(symbol_id)
                .map(|symbol| match symbol.kind {
                    SymbolKind::Variable { storage, .. } => {
                        matches!(storage, Storage::Local | Storage::Parameter | Storage::Global)
                    }
                    SymbolKind::Field { access, .. } => {
                        matches!(access, Access::Public | Access::Protected | Access::Private)
                    }
                    SymbolKind::Function { .. }
                    | SymbolKind::Method { .. }
                    | SymbolKind::Class { .. }
                    | SymbolKind::Enum { .. }
                    | SymbolKind::Typedef { .. }
                    | SymbolKind::Namespace { .. }
                    | SymbolKind::EnumValue { .. } => true,
                    SymbolKind::Template => false,
                })
                .unwrap_or(false)
        })
    }

    pub(crate) fn types_char(&self) -> TypeId {
        self.cached_char_t.unwrap_or(self.types.unknown_t)
    }

    pub(crate) fn types_pointer_to_char(&self) -> TypeId {
        self.cached_char_ptr_t.unwrap_or(self.types.unknown_t)
    }
}
