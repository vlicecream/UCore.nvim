use std::collections::HashMap;

use tree_sitter::Node;

use super::builtin::builtin_type_by_name;
use super::scope::{ScopeId, ScopeKind, ScopeTree};
use super::symbol::{
    Access, ClassId, FuncOverload, SourceRef, Storage, SymbolKind, SymbolTable,
};
use super::types::{CvQual, RefKind, TemplateArg, TypeArena, TypeId, TypeKind};
use super::SemaContext;

pub fn build_sema(root: Node, source: &str) -> SemaContext {
    let mut builder = SemaBuilder {
        source,
        types: TypeArena::new(),
        symbols: SymbolTable::new(),
        scopes: ScopeTree::new(),
        node_scopes: HashMap::new(),
    };
    let global = builder.scopes.global;
    builder.attach_scope(root, global);
    builder.walk(root, global, None);
    builder.finish()
}

struct SemaBuilder<'a> {
    source: &'a str,
    types: TypeArena,
    symbols: SymbolTable,
    scopes: ScopeTree,
    node_scopes: HashMap<(usize, usize), ScopeId>,
}

impl<'a> SemaBuilder<'a> {
    fn finish(self) -> SemaContext {
        let mut ctx = SemaContext {
            types: self.types,
            symbols: self.symbols,
            scopes: self.scopes,
            node_scopes: self.node_scopes,
            source: Some(self.source.to_string()),
            cached_char_t: None,
            cached_char_ptr_t: None,
        };
        super::expr::attach_builtin_helpers(&mut ctx);
        ctx
    }

    fn walk(&mut self, node: Node, scope_id: ScopeId, current_class: Option<ClassId>) {
        match node.kind() {
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|name| self.node_text(name).trim().to_string())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "<anonymous>".to_string());
                let child_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Namespace);
                self.attach_scope(node, child_scope);
                self.add_symbol(scope_id, name, SymbolKind::Namespace { children: child_scope });
                self.walk_children(node, child_scope, current_class);
                return;
            }
            "class_specifier"
            | "struct_specifier"
            | "unreal_reflected_class_declaration"
            | "unreal_reflected_struct_declaration" => {
                if let Some(name_node) = class_like_name_node(node) {
                    let name = self.node_text(name_node).trim().to_string();
                    if !name.is_empty() {
                        let class_id = self.symbols.new_class_id(&name);
                        let type_id = self.types.intern(TypeKind::Class(class_id));
                        let class_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Class);
                        self.symbols.set_class_scope(class_id, class_scope);
                        self.attach_scope(node, class_scope);
                        self.add_symbol(
                            scope_id,
                            name,
                            SymbolKind::Class {
                                class_id,
                                type_id,
                                parents: Vec::new(),
                                is_struct: node.kind().contains("struct"),
                            },
                        );
                        self.walk_children(node, class_scope, Some(class_id));
                        return;
                    }
                }
            }
            "enum_specifier" | "unreal_reflected_enum_declaration" => {
                if let Some(name_node) = class_like_name_node(node) {
                    let name = self.node_text(name_node).trim().to_string();
                    if !name.is_empty() {
                        let enum_id = self.symbols.new_enum_id(&name);
                        let type_id = self.types.intern(TypeKind::Enum(enum_id));
                        self.add_symbol(scope_id, name, SymbolKind::Enum { enum_id, type_id });
                    }
                }
            }
            "function_definition" | "unreal_function_definition" => {
                self.register_function(node, scope_id, current_class, true);
                return;
            }
            "declaration" | "field_declaration" => {
                if self.is_function_like_declaration(node) {
                    self.register_function(node, scope_id, current_class, false);
                    return;
                }
                self.register_variable_like(node, scope_id, current_class, Storage::Local);
            }
            "parameter_declaration" => {
                self.register_variable_like(node, scope_id, current_class, Storage::Parameter);
                return;
            }
            "compound_statement" => {
                let block_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Block);
                self.attach_scope(node, block_scope);
                self.walk_children(node, block_scope, current_class);
                return;
            }
            "lambda_expression" => {
                let lambda_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Function);
                self.attach_scope(node, lambda_scope);
                self.walk_children(node, lambda_scope, current_class);
                return;
            }
            _ => {}
        }

        self.walk_children(node, scope_id, current_class);
    }

    fn walk_children(&mut self, node: Node, scope_id: ScopeId, current_class: Option<ClassId>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child, scope_id, current_class);
        }
    }

    fn register_function(
        &mut self,
        node: Node,
        scope_id: ScopeId,
        current_class: Option<ClassId>,
        is_definition: bool,
    ) {
        let Some(declarator) = node
            .child_by_field_name("declarator")
            .or_else(|| find_descendant(node, "function_declarator"))
        else {
            self.walk_children(node, scope_id, current_class);
            return;
        };

        let Some(name_node) = find_name_node(declarator) else {
            self.walk_children(node, scope_id, current_class);
            return;
        };
        let name = self.node_text(name_node).trim().to_string();
        if name.is_empty() {
            self.walk_children(node, scope_id, current_class);
            return;
        }

        let return_t = node
            .child_by_field_name("type")
            .map(|type_node| self.resolve_type_node(type_node))
            .unwrap_or(self.types.unknown_t);
        let params = function_parameter_types(self, declarator);
        let fn_type = self.types.intern(TypeKind::Function {
            return_t,
            params,
            is_variadic: self.node_text(declarator).contains("..."),
            is_const_member: self.node_text(node).contains(" const"),
        });

        let kind = if let Some(owner) = current_class {
            SymbolKind::Method {
                owner,
                type_id: fn_type,
                access: Access::Public,
                is_static: self.node_text(node).contains("static "),
                is_virtual: self.node_text(node).contains("virtual "),
                is_override: self.node_text(node).contains(" override"),
            }
        } else {
            SymbolKind::Function {
                decls: vec![FuncOverload {
                    type_id: fn_type,
                    source: SourceRef {
                        line: node.start_position().row as u32,
                        column: node.start_position().column as u32,
                    },
                    is_definition,
                }],
            }
        };
        self.add_symbol(scope_id, name, kind);

        let fn_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Function);
        self.attach_scope(node, fn_scope);
        self.register_parameters(declarator, fn_scope, current_class);

        if let Some(body) = find_descendant(node, "compound_statement") {
            self.walk(body, fn_scope, current_class);
        }
    }

    fn register_parameters(
        &mut self,
        declarator: Node,
        scope_id: ScopeId,
        current_class: Option<ClassId>,
    ) {
        let Some(params) = find_descendant(declarator, "parameter_list") else {
            return;
        };
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                self.register_variable_like(child, scope_id, current_class, Storage::Parameter);
            }
        }
    }

    fn register_variable_like(
        &mut self,
        node: Node,
        scope_id: ScopeId,
        current_class: Option<ClassId>,
        storage: Storage,
    ) {
        let Some(name_node) = node
            .child_by_field_name("declarator")
            .and_then(find_name_node)
            .or_else(|| find_name_node(node))
        else {
            return;
        };
        let name = self.node_text(name_node).trim().to_string();
        if name.is_empty() {
            return;
        }

        let type_id = node
            .child_by_field_name("type")
            .map(|type_node| {
                let base = self.resolve_type_node(type_node);
                let full_text = self.node_text(node).to_string();
                apply_declarator_wrappers(
                    &mut self.types,
                    base,
                    node.child_by_field_name("declarator"),
                    &full_text,
                )
            })
            .unwrap_or(self.types.unknown_t);

        let kind = match (current_class, node.kind(), storage) {
            (_, "parameter_declaration", _) => SymbolKind::Variable {
                type_id,
                storage: Storage::Parameter,
            },
            (Some(owner), "field_declaration", _) => SymbolKind::Field {
                owner,
                type_id,
                access: Access::Public,
            },
            (_, _, storage) => SymbolKind::Variable { type_id, storage },
        };

        self.add_symbol(scope_id, name, kind);
    }

    fn resolve_type_node(&mut self, node: Node) -> TypeId {
        match node.kind() {
            "primitive_type" => {
                let text = self.node_text(node).to_string();
                builtin_type_by_name(&mut self.types, &text).unwrap_or(self.types.unknown_t)
            }
            "type_identifier" | "sized_type_specifier" => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                builtin_type_by_name(&mut self.types, &text).unwrap_or_else(|| {
                    let class_id = self.symbols.new_class_id(&text);
                    self.types.intern(TypeKind::Class(class_id))
                })
            }
            "qualified_identifier" => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                let class_id = self.symbols.new_class_id(&text);
                self.types.intern(TypeKind::Class(class_id))
            }
            "template_type" => {
                let base = node
                    .child_by_field_name("name")
                    .map(|name| self.node_text(name).trim().to_string())
                    .unwrap_or_else(|| self.node_text(node).trim().to_string());
                let mut args = Vec::new();
                if let Some(arg_list) = find_descendant(node, "template_argument_list") {
                    let mut cursor = arg_list.walk();
                    for child in arg_list.children(&mut cursor) {
                        match child.kind() {
                            "type_descriptor"
                            | "type_identifier"
                            | "template_type"
                            | "qualified_identifier" => {
                                args.push(TemplateArg::Type(self.resolve_type_node(child)));
                            }
                            _ => {}
                        }
                    }
                }
                self.types.intern(TypeKind::Template { base, args })
            }
            _ => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                builtin_type_by_name(&mut self.types, &text).unwrap_or(self.types.unknown_t)
            }
        }
    }

    fn add_symbol(&mut self, scope_id: ScopeId, name: String, kind: SymbolKind) {
        let symbol_id = self.symbols.add_symbol(name.clone(), scope_id, kind);
        if let Some(scope) = self.scopes.get_mut(scope_id) {
            scope.symbols.entry(name).or_default().push(symbol_id);
        }
    }

    fn attach_scope(&mut self, node: Node, scope_id: ScopeId) {
        self.node_scopes
            .insert((node.start_byte(), node.end_byte()), scope_id);
    }

    fn is_function_like_declaration(&self, node: Node) -> bool {
        node.child_by_field_name("declarator")
            .and_then(|declarator| find_descendant(declarator, "function_declarator"))
            .is_some()
    }

    fn node_text(&self, node: Node) -> &str {
        let range = node.byte_range();
        if range.end <= self.source.len()
            && self.source.is_char_boundary(range.start)
            && self.source.is_char_boundary(range.end)
        {
            &self.source[range.start..range.end]
        } else {
            ""
        }
    }
}

fn function_parameter_types(builder: &mut SemaBuilder<'_>, declarator: Node) -> Vec<TypeId> {
    let Some(params) = find_descendant(declarator, "parameter_list") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            let type_id = child
                .child_by_field_name("type")
                .map(|type_node| {
                    let base = builder.resolve_type_node(type_node);
                    let full_text = builder.node_text(child).to_string();
                    apply_declarator_wrappers(
                        &mut builder.types,
                        base,
                        child.child_by_field_name("declarator"),
                        &full_text,
                    )
                })
                .unwrap_or(builder.types.unknown_t);
            out.push(type_id);
        }
    }
    out
}

fn apply_declarator_wrappers(
    arena: &mut TypeArena,
    mut current: TypeId,
    declarator: Option<Node>,
    full_text: &str,
) -> TypeId {
    let mut cursor = declarator;
    while let Some(node) = cursor {
        current = match node.kind() {
            "pointer_declarator" => arena.intern(TypeKind::Pointer {
                pointee: current,
                cv: CvQual::default(),
            }),
            "reference_declarator" => arena.intern(TypeKind::Reference {
                referent: current,
                kind: RefKind::LValue,
            }),
            "array_declarator" => arena.intern(TypeKind::Array {
                elem: current,
                size: None,
            }),
            _ => current,
        };
        cursor = node.child_by_field_name("declarator");
    }

    if full_text.contains('*') {
        current = arena.intern(TypeKind::Pointer {
            pointee: current,
            cv: CvQual::default(),
        });
    } else if full_text.contains('&') {
        current = arena.intern(TypeKind::Reference {
            referent: current,
            kind: RefKind::LValue,
        });
    }

    current
}

fn class_like_name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
        .or_else(|| find_direct_child_by_kind(node, &["type_identifier", "identifier"]))
}

fn find_direct_child_by_kind<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| kinds.iter().any(|kind| child.kind() == *kind))
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

fn find_name_node(node: Node) -> Option<Node> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(node),
        "qualified_identifier" => node.child_by_field_name("name"),
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "init_declarator"
        | "bitfield_clause" => node
            .child_by_field_name("declarator")
            .and_then(find_name_node),
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
