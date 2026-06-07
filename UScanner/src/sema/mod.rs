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

#[derive(Clone, Debug)]
pub enum TemplateScopeParam {
    Type { name: String },
    Value { name: String, declared_type: String },
}

impl TemplateScopeParam {
    fn name(&self) -> &str {
        match self {
            TemplateScopeParam::Type { name } | TemplateScopeParam::Value { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemberCallableSignature {
    pub return_t: TypeId,
    pub params: Vec<TypeId>,
    pub min_arity: usize,
    pub is_variadic: bool,
    pub is_const_member: bool,
}

pub struct SemaContext {
    pub types: TypeArena,
    pub symbols: SymbolTable,
    pub scopes: ScopeTree,
    pub node_scopes: HashMap<(usize, usize), ScopeId>,
    pub scope_owner_symbols: HashMap<ScopeId, SymbolId>,
    pub class_template_params: HashMap<symbol::ClassId, Vec<TemplateScopeParam>>,
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

    fn find_first_kind<'a>(
        node: tree_sitter::Node<'a>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_first_kind(child, kind) {
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

    #[test]
    fn sema_expr_resolves_subscript_element_type() {
        let content = "int32 Test(int32* Values) { return Values[0]; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "subscript_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_lookup_resolves_namespace_qualified_function() {
        let content = "namespace UE::Math { int32 Value(); } int32 Test() { return UE::Math::Value(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "qualified_identifier").unwrap();
        let symbols = sema.lookup_qualified_name_at_node(node, &["UE", "Math", "Value"]);
        assert!(!symbols.is_empty());
    }

    #[test]
    fn sema_expr_resolves_namespace_qualified_call_return_type() {
        let content = "namespace UE::Math { int32 Value(); } int32 Test() { return UE::Math::Value(); }";
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

    #[test]
    fn sema_lookup_resolves_using_declaration_target() {
        let content =
            "namespace UE::Math { int32 Value(); } using UE::Math::Value; int32 Test() { return Value(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_identifier(root, "Value", content).unwrap();
        assert!(sema.symbol_exists_at_node(node, "Value"));
    }

    #[test]
    fn sema_lookup_resolves_using_namespace_target() {
        let content =
            "namespace UE::Math { int32 Value(); } using namespace UE::Math; int32 Test() { return Value(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_identifier(root, "Value", content).unwrap();
        assert!(sema.symbol_exists_at_node(node, "Value"));
    }

    #[test]
    fn sema_expr_resolves_typedef_alias_variable_type() {
        let content = "using FCount = int32; void Test() { FCount Value = 1; Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_identifier(root, "Value", content).unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "FCount");
    }

    #[test]
    fn sema_expr_resolves_member_on_namespace_qualified_class_type() {
        let content =
            "namespace UE::Math { class Vec { public: int32 X; }; } void Test() { UE::Math::Vec Value; Value.X; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_named_cast_call_target_type() {
        let content = "void Test() { auto Value = static_cast<float>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "call_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "float");
    }

    #[test]
    fn sema_expr_resolves_adl_call_return_type() {
        let content =
            "namespace UE::Math { struct Vec {}; int32 Length(Vec Value) { return 1; } } int32 Test() { UE::Math::Vec Value; return Length(Value); }";
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

    #[test]
    fn sema_expr_resolves_nullptr_type() {
        let content = "void Test() { nullptr; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "null").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "nullptr");
    }

    #[test]
    fn sema_expr_resolves_pointer_alias_dereference_type() {
        let content = "class UFoo {}; using FPtr = UFoo*; void Test() { FPtr Value = nullptr; *Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "pointer_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "UFoo");
    }

    #[test]
    fn sema_expr_resolves_pointer_alias_subscript_type() {
        let content = "using FIntPtr = int32*; int32 Test(FIntPtr Values) { return Values[0]; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "subscript_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_member_on_template_class_instance() {
        let content =
            "template<typename T> struct Box { int32 Value; }; void Test() { Box<int32> Item; Item.Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_method_call_on_template_class_instance() {
        let content =
            "template<typename T> struct Box { int32 Get() { return 1; } }; int32 Test() { Box<int32> Item; return Item.Get(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "call_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_method_call_on_namespace_qualified_template_class_instance() {
        let content =
            "namespace UE::Math { template<typename T> struct Box { int32 Get() { return 1; } }; } int32 Test() { UE::Math::Box<int32> Item; return Item.Get(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "call_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_member_on_explicit_template_specialization_instance() {
        let content =
            "class UObject {}; template<typename T> struct Box { int32 Value; }; template<> struct Box<int32> { UObject* Value; }; UObject* Test() { Box<int32> Item; return Item.Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "UObject*");
    }

    #[test]
    fn sema_expr_keeps_primary_template_member_on_non_specialized_instance() {
        let content =
            "class UObject {}; template<typename T> struct Box { int32 Value; }; template<> struct Box<int32> { UObject* Value; }; int32 Test() { Box<float> Item; return Item.Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_dependent_template_field_type_from_instance_arg() {
        let content =
            "template<typename T> struct Box { T Value; }; int32 Test() { Box<int32> Item; return Item.Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_dependent_template_method_signature_from_instance_arg() {
        let content =
            "template<typename T> struct Box { T Get(T Value) { return Value; } }; int32 Test() { Box<int32> Item; return Item.Get(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "call_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn sema_expr_resolves_pointer_dependent_template_field_type() {
        let content =
            "template<typename T> struct Box { T* Value; }; void Test() { Box<int32> Item; Item.Value; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32*");
    }

    #[test]
    fn sema_expr_resolves_reference_dependent_template_method_signature() {
        let content =
            "template<typename T> struct Box { T& Get(T& Value) { return Value; } }; void Test(int32& Ref) { Box<int32> Item; Item.Get(Ref); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "call_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32&");
    }

    #[test]
    fn sema_expr_resolves_reference_parameter_identifier_type() {
        let content = "void Test(int32& Ref) { Ref; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let stmt = find_first_kind(root, "expression_statement").unwrap();
        let node = stmt.named_child(0).unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32&");
    }

    #[test]
    fn sema_lookup_finds_dependent_template_method_member_symbol() {
        let content =
            "template<typename T> struct Box { T& Get(T& Value) { return Value; } }; void Test(int32& Ref) { Box<int32> Item; Item.Get(Ref); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let call = find_first_kind(root, "call_expression").unwrap();
        let callee = call.child_by_field_name("function").unwrap();
        let receiver = callee.child_by_field_name("argument").unwrap();
        let receiver_ty = type_of_expression(&sema, receiver).unwrap();
        let symbols = sema.lookup_class_member_symbols(receiver_ty, "Get");
        assert_eq!(symbols.len(), 1);
    }

    #[test]
    fn sema_expr_resolves_nested_template_dependent_field_type() {
        let content =
            "template<typename U> struct TArray {}; template<typename T> struct Box { TArray<T> Values; }; void Test() { Box<int32> Item; Item.Values; }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "field_expression").unwrap();
        let ty = type_of_expression(&sema, node)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "TArray<int32>");
    }

    #[test]
    fn sema_expr_resolves_nested_pointer_reference_template_deref_type() {
        let content =
            "template<typename T> T* Id(T*& Value) { return Value; } int32 Test(int32* Ptr) { return *Id(Ptr); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let node = find_first_kind(root, "pointer_expression").unwrap();
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

    pub fn lookup_call_name_at_node(
        &self,
        node: Node,
        name: &str,
        arg_types: &[TypeId],
    ) -> Vec<SymbolId> {
        let scope = self.scope_for_node(node);
        lookup::lookup_call_name(self, scope, name, arg_types)
    }

    pub fn lookup_qualified_name_at_node(&self, node: Node, segments: &[&str]) -> Vec<SymbolId> {
        let scope = self.scope_for_node(node);
        lookup::lookup_qualified_name(self, scope, segments)
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

    pub fn resolve_qualified_symbol_at_node(&self, node: Node, segments: &[&str]) -> Option<SymbolId> {
        self.lookup_qualified_name_at_node(node, segments)
            .into_iter()
            .next()
    }

    pub fn type_of_identifier_at_node(&self, node: Node, name: &str) -> Option<TypeId> {
        self.lookup_name_at_node(node, name)
            .into_iter()
            .find_map(|symbol_id| self.symbol_type(symbol_id))
    }

    pub fn type_of_qualified_identifier_at_node(&self, node: Node, segments: &[&str]) -> Option<TypeId> {
        self.lookup_qualified_name_at_node(node, segments)
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
            TypeKind::Template { base, .. } => {
                if let Some(rendered) = self.render_type(type_id)
                    && let Some(class_id) = self.symbols.class_id_by_name(&rendered)
                {
                    return Some(class_id);
                }
                self.resolve_existing_type_text(base)
                    .and_then(|resolved| self.class_id_for_type(resolved))
            }
            TypeKind::Pointer { pointee, .. } => self.class_id_for_type(*pointee),
            TypeKind::Reference { referent, .. } => self.class_id_for_type(*referent),
            TypeKind::Typedef { aliased, .. } => self.class_id_for_type(*aliased),
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

    pub fn member_symbol_type(&self, receiver_type: TypeId, symbol_id: SymbolId) -> Option<TypeId> {
        let symbol = self.symbols.get(symbol_id)?;
        let base_type = self.symbol_type(symbol_id)?;
        let owner = match symbol.kind {
            SymbolKind::Field { owner, .. } | SymbolKind::Method { owner, .. } => owner,
            _ => return Some(base_type),
        };
        let Some(bindings) = self.template_bindings_for_member_owner(receiver_type, owner) else {
            return Some(base_type);
        };
        self.substitute_type_id(base_type, &bindings).or(Some(base_type))
    }

    pub fn member_callable_signature(
        &self,
        receiver_type: TypeId,
        symbol_id: SymbolId,
    ) -> Option<MemberCallableSignature> {
        let symbol = self.symbols.get(symbol_id)?;
        let SymbolKind::Method { owner, type_id, .. } = symbol.kind else {
            let type_id = self.symbol_type(symbol_id)?;
            let TypeKind::Function {
                return_t,
                params,
                min_arity,
                is_variadic,
                is_const_member,
            } = self.types.get(type_id)?
            else {
                return None;
            };
            return Some(MemberCallableSignature {
                return_t: *return_t,
                params: params.clone(),
                min_arity: *min_arity,
                is_variadic: *is_variadic,
                is_const_member: *is_const_member,
            });
        };

        let TypeKind::Function {
            return_t,
            params,
            min_arity,
            is_variadic,
            is_const_member,
        } = self.types.get(type_id)?
        else {
            return None;
        };

        let Some(bindings) = self.template_bindings_for_member_owner(receiver_type, owner) else {
            return Some(MemberCallableSignature {
                return_t: *return_t,
                params: params.clone(),
                min_arity: *min_arity,
                is_variadic: *is_variadic,
                is_const_member: *is_const_member,
            });
        };

        Some(MemberCallableSignature {
            return_t: self.substitute_type_id(*return_t, &bindings).unwrap_or(*return_t),
            params: params
                .iter()
                .map(|param| self.substitute_type_id(*param, &bindings).unwrap_or(*param))
                .collect(),
            min_arity: *min_arity,
            is_variadic: *is_variadic,
            is_const_member: *is_const_member,
        })
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
            TypeKind::Nullptr => "nullptr".to_string(),
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

    pub fn resolve_existing_type_text(&self, raw: &str) -> Option<TypeId> {
        let clean = crate::parser::cpp::clean_type_string(raw);
        self.resolve_existing_clean_type(&clean)
    }

    pub fn resolve_existing_type_node(&self, node: Node) -> Option<TypeId> {
        let source = self.source()?;
        let range = node.byte_range();
        if range.end > source.len()
            || !source.is_char_boundary(range.start)
            || !source.is_char_boundary(range.end)
        {
            return None;
        }
        self.resolve_existing_type_text(&source[range.start..range.end])
    }

    pub fn find_pointer_type(&self, pointee: TypeId) -> Option<TypeId> {
        self.types.find(&TypeKind::Pointer {
            pointee,
            cv: types::CvQual::default(),
        }).or_else(|| {
            let rendered = self.render_type(pointee)?;
            self.types.iter().find_map(|(type_id, kind)| match kind {
                TypeKind::Pointer { pointee, .. }
                    if self.render_type(*pointee).as_deref() == Some(rendered.as_str()) =>
                {
                    Some(type_id)
                }
                _ => None,
            })
        })
    }

    pub fn find_reference_type(&self, referent: TypeId) -> Option<TypeId> {
        self.types.find(&TypeKind::Reference {
            referent,
            kind: types::RefKind::LValue,
        })
    }

    pub fn find_array_type(&self, elem: TypeId) -> Option<TypeId> {
        self.types.find(&TypeKind::Array { elem, size: None })
    }

    pub fn find_function_type(
        &self,
        return_t: TypeId,
        params: Vec<TypeId>,
        min_arity: usize,
        is_variadic: bool,
        is_const_member: bool,
    ) -> Option<TypeId> {
        self.types.find(&TypeKind::Function {
            return_t,
            params,
            min_arity,
            is_variadic,
            is_const_member,
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
        let from = self.canonical_type_id(from);
        let to = self.canonical_type_id(to);
        if from == to {
            return Compat::Same;
        }

        let from_ty = self.types.get(from);
        let to_ty = self.types.get(to);
        match (from_ty, to_ty) {
            (Some(TypeKind::Nullptr), Some(TypeKind::Reference { referent, .. })) => {
                self.check_compat(from, *referent)
            }
            (Some(TypeKind::Nullptr), Some(TypeKind::Pointer { .. })) => Compat::NullptrToPointer,
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
            (Some(TypeKind::Reference { referent, .. }), _) => self.check_compat(*referent, to),
            (_, Some(TypeKind::Reference { referent, .. })) => self.check_compat(from, *referent),
            (
                Some(TypeKind::Class(from_class)),
                Some(TypeKind::Class(to_class)),
            ) if self.is_class_derived_from_class(*from_class, *to_class) => Compat::DerivedToBase,
            _ => Compat::Incompatible,
        }
    }

    pub fn types_equivalent(&self, left: TypeId, right: TypeId) -> bool {
        matches!(self.check_compat(left, right), Compat::Same)
            && matches!(self.check_compat(right, left), Compat::Same)
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

    fn resolve_existing_clean_type(&self, clean: &str) -> Option<TypeId> {
        let clean = clean.trim();
        if clean.is_empty() {
            return None;
        }

        if let Some(inner) = clean.strip_suffix("[]") {
            let elem = self.resolve_existing_clean_type(inner)?;
            return self.find_array_type(elem);
        }
        if let Some(inner) = clean.strip_suffix('&') {
            let referent = self.resolve_existing_clean_type(inner.trim_end())?;
            return self.find_reference_type(referent);
        }
        if let Some(inner) = clean.strip_suffix('*') {
            let pointee = self.resolve_existing_clean_type(inner.trim_end())?;
            return self.find_pointer_type(pointee);
        }
        if let Some(template_type) = self.resolve_existing_template_type(clean) {
            return Some(template_type);
        }

        match clean {
            "void" => Some(self.types.void_t),
            "bool" => Some(self.types.bool_t),
            "char" | "ANSICHAR" | "TCHAR" => Some(self.types_char()),
            "int" | "int32" => Some(self.types.int32_t),
            "uint32" => Some(self.types.uint32_t),
            "float" => Some(self.types.float_t),
            "double" => Some(self.types.double_t),
            _ => self.lookup_typedef_type_by_name(clean).or_else(|| {
                if clean.contains("::") {
                    self.lookup_qualified_type_by_name(clean)
                } else {
                    self.symbols
                        .class_id_by_name(clean)
                        .and_then(|class_id| self.types.find(&TypeKind::Class(class_id)))
                }
            }),
        }
    }

    fn resolve_existing_template_type(&self, clean: &str) -> Option<TypeId> {
        let name_end = clean.find('<')?;
        let base = clean[..name_end].trim();
        let args_text = clean.strip_prefix(base)?.trim();
        let inner = args_text.strip_prefix('<')?.strip_suffix('>')?;
        let mut args = Vec::new();
        for arg in split_template_args(inner) {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(type_id) = self.resolve_existing_clean_type(trimmed) {
                args.push(types::TemplateArg::Type(type_id));
            } else {
                args.push(types::TemplateArg::Value(trimmed.to_string()));
            }
        }
        self.types.find(&TypeKind::Template {
            base: base.to_string(),
            args,
        })
    }

    fn lookup_typedef_type_by_name(&self, name: &str) -> Option<TypeId> {
        for scope in self.scopes.iter() {
            let Some(ids) = scope.symbols.get(name) else {
                continue;
            };
            for symbol_id in ids {
                let symbol = self.symbols.get(*symbol_id)?;
                if let SymbolKind::Typedef { type_id } = symbol.kind {
                    return Some(type_id);
                }
            }
        }
        None
    }

    fn lookup_qualified_type_by_name(&self, name: &str) -> Option<TypeId> {
        let segments = name
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let refs = segments.to_vec();
        lookup::lookup_qualified_name(self, self.scopes.global, &refs)
            .into_iter()
            .find_map(|symbol_id| {
                let symbol = self.symbols.get(symbol_id)?;
                match symbol.kind {
                    SymbolKind::Class { type_id, .. }
                    | SymbolKind::Enum { type_id, .. }
                    | SymbolKind::Typedef { type_id } => Some(type_id),
                    _ => None,
                }
            })
    }

    fn canonical_type_id(&self, type_id: TypeId) -> TypeId {
        let mut current = type_id;
        let mut guard = 0usize;
        while guard < 32 {
            match self.types.get(current) {
                Some(TypeKind::Typedef { aliased, .. }) => current = *aliased,
                _ => return current,
            }
            guard += 1;
        }
        current
    }

    fn template_bindings_for_member_owner(
        &self,
        receiver_type: TypeId,
        owner: symbol::ClassId,
    ) -> Option<HashMap<String, types::TemplateArg>> {
        let params = self.class_template_params.get(&owner)?;
        if params.is_empty() {
            return None;
        }

        let receiver = self.canonical_type_id(receiver_type);
        let receiver_kind = match self.types.get(receiver)? {
            TypeKind::Template { args, .. } => Some(args),
            TypeKind::Pointer { pointee, .. } => match self.types.get(*pointee)? {
                TypeKind::Template { args, .. } => Some(args),
                _ => None,
            },
            TypeKind::Reference { referent, .. } => match self.types.get(*referent)? {
                TypeKind::Template { args, .. } => Some(args),
                _ => None,
            },
            _ => None,
        }?;

        let mut bindings = HashMap::new();
        for (param, arg) in params.iter().zip(receiver_kind.iter()) {
            bindings.insert(param.name().to_string(), arg.clone());
        }
        Some(bindings)
    }

    fn substitute_type_id(
        &self,
        type_id: TypeId,
        bindings: &HashMap<String, types::TemplateArg>,
    ) -> Option<TypeId> {
        match self.types.get(type_id)? {
            TypeKind::Dependent(name) => match bindings.get(name) {
                Some(types::TemplateArg::Type(type_id)) => Some(*type_id),
                _ => Some(type_id),
            },
            TypeKind::Pointer { pointee, .. } => {
                let pointee = self.substitute_type_id(*pointee, bindings)?;
                self.find_pointer_type(pointee)
            }
            TypeKind::Reference { referent, kind } => {
                let referent = self.substitute_type_id(*referent, bindings)?;
                match kind {
                    types::RefKind::LValue => self.find_reference_type(referent),
                    types::RefKind::RValue => self.types.find(&TypeKind::Reference {
                        referent,
                        kind: *kind,
                    }),
                }
            }
            TypeKind::Array { elem, size } => {
                let elem = self.substitute_type_id(*elem, bindings)?;
                self.types.find(&TypeKind::Array {
                    elem,
                    size: *size,
                })
            }
            TypeKind::Template { base, args } => {
                let substituted_args = args
                    .iter()
                    .map(|arg| match arg {
                        types::TemplateArg::Type(type_id) => self
                            .substitute_type_id(*type_id, bindings)
                            .map(types::TemplateArg::Type),
                        types::TemplateArg::Value(value) => Some(match bindings.get(value) {
                            Some(types::TemplateArg::Type(type_id)) => {
                                types::TemplateArg::Type(*type_id)
                            }
                            Some(types::TemplateArg::Value(value)) => {
                                types::TemplateArg::Value(value.clone())
                            }
                            None => types::TemplateArg::Value(value.clone()),
                        }),
                    })
                    .collect::<Option<Vec<_>>>()?;
                self.types.find(&TypeKind::Template {
                    base: base.clone(),
                    args: substituted_args,
                })
            }
            _ => Some(type_id),
        }
    }
}

fn split_template_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
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
