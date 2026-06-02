pub mod builtin;
pub mod builder;
pub mod cfg;
pub mod dataflow;
pub mod expr;
pub mod lookup;
pub mod overload;
pub mod scope;
pub mod symbol;
pub mod template;
pub mod types;

use std::collections::HashMap;

use tree_sitter::Node;

use scope::{ScopeId, ScopeTree};
use symbol::{Access, Storage, SymbolId, SymbolKind, SymbolTable};
use types::{BuiltinType, Compat, TypeArena, TypeId, TypeKind};

pub struct SemaContext {
    pub types: TypeArena,
    pub symbols: SymbolTable,
    pub scopes: ScopeTree,
    pub node_scopes: HashMap<(usize, usize), ScopeId>,
    pub scope_owner_symbols: HashMap<ScopeId, SymbolId>,
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

    #[test]
    fn sema_expr_resolves_template_call_return_type() {
        let content =
            "template<typename T> T Id(T Value) { return Value; } void Test() { Id<int32>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);

        fn find_call(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
            if node.kind() == "call_expression" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_call(child) {
                    return Some(found);
                }
            }
            None
        }

        let call = find_call(root).unwrap();
        let ty = type_of_expression(&sema, call)
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

    pub fn lookup_class_member_type(&self, class_type: TypeId, member_name: &str) -> Option<TypeId> {
        self.lookup_class_member_symbols(class_type, member_name)
            .into_iter()
            .find_map(|symbol_id| self.symbol_type(symbol_id))
    }

    pub fn lookup_member_on_class_id(&self, class_id: symbol::ClassId, member_name: &str) -> Option<TypeId> {
        self.lookup_member_on_class_id_symbols(class_id, member_name)
            .into_iter()
            .find_map(|symbol_id| self.symbol_type(symbol_id))
    }

    pub fn lookup_class_member_symbols(&self, class_type: TypeId, member_name: &str) -> Vec<SymbolId> {
        let Some(class_id) = self.class_id_for_type(class_type) else {
            return Vec::new();
        };
        self.lookup_member_on_class_id_symbols(class_id, member_name)
    }

    pub fn class_id_for_type(&self, type_id: TypeId) -> Option<symbol::ClassId> {
        match self.types.get(type_id)? {
            TypeKind::Class(class_id) => Some(*class_id),
            TypeKind::Pointer { pointee, .. } => self.class_id_for_type(*pointee),
            TypeKind::Reference { referent, .. } => self.class_id_for_type(*referent),
            _ => None,
        }
    }

    pub fn enclosing_function_return_type(&self, node: Node) -> Option<TypeId> {
        let mut scope_id = Some(self.scope_for_node(node));
        while let Some(current) = scope_id {
            if let Some(symbol_id) = self.scope_owner_symbols.get(&current).copied() {
                let symbol = self.symbols.get(symbol_id)?;
                match &symbol.kind {
                    SymbolKind::Function { decls } => {
                        let fn_type = decls.first()?.type_id;
                        let TypeKind::Function { return_t, .. } = self.types.get(fn_type)? else {
                            return None;
                        };
                        return Some(*return_t);
                    }
                    SymbolKind::Method { type_id, .. } => {
                        let TypeKind::Function { return_t, .. } = self.types.get(*type_id)? else {
                            return None;
                        };
                        return Some(*return_t);
                    }
                    _ => {}
                }
            }
            scope_id = self.scopes.get(current).and_then(|scope| scope.parent);
        }
        None
    }

    fn lookup_member_on_class_id_symbols(
        &self,
        class_id: symbol::ClassId,
        member_name: &str,
    ) -> Vec<SymbolId> {
        let mut results = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.collect_member_symbols(class_id, member_name, &mut visited, &mut results);
        results
    }

    fn collect_member_symbols(
        &self,
        class_id: symbol::ClassId,
        member_name: &str,
        visited: &mut std::collections::HashSet<symbol::ClassId>,
        results: &mut Vec<SymbolId>,
    ) {
        if !visited.insert(class_id) {
            return;
        }

        if let Some(scope_id) = self.symbols.class_scope(class_id) {
            if let Some(scope) = self.scopes.get(scope_id) {
                if let Some(ids) = scope.symbols.get(member_name) {
                    results.extend(ids.iter().copied());
                }
            }
        }

        for parent in self.symbols.class_parents(class_id) {
            if let Some(TypeKind::Class(parent_id)) = self.types.get(*parent) {
                self.collect_member_symbols(*parent_id, member_name, visited, results);
            }
        }
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
            TypeKind::Template { base, args } => {
                let rendered_args = args
                    .iter()
                    .map(|arg| match arg {
                        types::TemplateArg::Type(type_id) => self
                            .render_type(*type_id)
                            .unwrap_or_else(|| "unknown".to_string()),
                        types::TemplateArg::Value(value) => value.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{base}<{rendered_args}>")
            }
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

    pub fn check_compat(&self, from: TypeId, to: TypeId) -> Compat {
        if from == to {
            return Compat::Same;
        }

        let from_ty = self.types.get(from);
        let to_ty = self.types.get(to);
        match (from_ty, to_ty) {
            (Some(TypeKind::Builtin(from_builtin)), Some(TypeKind::Builtin(to_builtin))) => {
                if is_numeric(*from_builtin) && is_numeric(*to_builtin) {
                    if numeric_rank(*from_builtin) <= numeric_rank(*to_builtin) {
                        Compat::NumericPromote
                    } else {
                        Compat::NumericConvert
                    }
                } else {
                    Compat::Incompatible
                }
            }
            (
                Some(TypeKind::Pointer { pointee: from_pointee, .. }),
                Some(TypeKind::Pointer { pointee: to_pointee, .. }),
            ) => {
                if from_pointee == to_pointee {
                    Compat::Same
                } else if matches!(self.types.get(*to_pointee), Some(TypeKind::Builtin(BuiltinType::Void))) {
                    Compat::PointerToVoid
                } else if self.is_class_derived_from(*from_pointee, *to_pointee) {
                    Compat::DerivedToBase
                } else {
                    Compat::Incompatible
                }
            }
            (
                Some(TypeKind::Reference { referent: from_ref, .. }),
                Some(TypeKind::Reference { referent: to_ref, .. }),
            ) => self.check_compat(*from_ref, *to_ref),
            (
                Some(TypeKind::Class(from_class)),
                Some(TypeKind::Class(to_class)),
            ) if self.is_class_derived_from_class(*from_class, *to_class) => Compat::DerivedToBase,
            _ => Compat::Incompatible,
        }
    }

    fn is_class_derived_from(&self, from_type: TypeId, to_type: TypeId) -> bool {
        let (Some(TypeKind::Class(from_class)), Some(TypeKind::Class(to_class))) =
            (self.types.get(from_type), self.types.get(to_type))
        else {
            return false;
        };
        self.is_class_derived_from_class(*from_class, *to_class)
    }

    fn is_class_derived_from_class(
        &self,
        from_class: symbol::ClassId,
        to_class: symbol::ClassId,
    ) -> bool {
        if from_class == to_class {
            return true;
        }

        for parent in self.symbols.class_parents(from_class) {
            if let Some(TypeKind::Class(parent_class)) = self.types.get(*parent) {
                if *parent_class == to_class || self.is_class_derived_from_class(*parent_class, to_class) {
                    return true;
                }
            }
        }

        false
    }
}

fn is_numeric(kind: BuiltinType) -> bool {
    matches!(
        kind,
        BuiltinType::Char
            | BuiltinType::Int32
            | BuiltinType::UInt32
            | BuiltinType::Float
            | BuiltinType::Double
    )
}

fn numeric_rank(kind: BuiltinType) -> u8 {
    match kind {
        BuiltinType::Char => 1,
        BuiltinType::Int32 | BuiltinType::UInt32 => 2,
        BuiltinType::Float => 3,
        BuiltinType::Double => 4,
        BuiltinType::Void | BuiltinType::Bool => 0,
    }
}
