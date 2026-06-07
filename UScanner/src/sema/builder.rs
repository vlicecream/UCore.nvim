use std::collections::HashMap;

use tree_sitter::Node;

use super::builtin::builtin_type_by_name;
use super::scope::{ScopeId, ScopeKind, ScopeTree};
use super::symbol::{
    Access, ClassId, FuncOverload, SourceRef, Storage, SymbolKind, SymbolTable,
};
use super::TemplateScopeParam;
use super::types::{CvQual, RefKind, TemplateArg, TypeArena, TypeId, TypeKind};
use super::SemaContext;

pub fn build_sema(root: Node, source: &str) -> SemaContext {
    let mut builder = SemaBuilder {
        source,
        types: TypeArena::new(),
        symbols: SymbolTable::new(),
        scopes: ScopeTree::new(),
        node_scopes: HashMap::new(),
        scope_owner_symbols: HashMap::new(),
        class_template_params: HashMap::new(),
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
    scope_owner_symbols: HashMap<ScopeId, super::symbol::SymbolId>,
    class_template_params: HashMap<ClassId, Vec<TemplateScopeParam>>,
}

impl<'a> SemaBuilder<'a> {
    fn finish(self) -> SemaContext {
        let mut ctx = SemaContext {
            types: self.types,
            symbols: self.symbols,
            scopes: self.scopes,
            node_scopes: self.node_scopes,
            scope_owner_symbols: self.scope_owner_symbols,
            class_template_params: self.class_template_params,
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
                let child_scope = self.ensure_namespace_scope(scope_id, &name);
                self.attach_scope(node, child_scope);
                self.walk_children(node, child_scope, current_class);
                return;
            }
            "class_specifier"
            | "struct_specifier"
            | "unreal_reflected_class_declaration"
            | "unreal_reflected_struct_declaration" => {
                if let Some(name_node) = class_like_name_node(node) {
                    let local_name = self.node_text(name_node).trim().to_string();
                    if !local_name.is_empty() {
                        let (class_name, symbol_name) =
                            self.class_registration_names(node, scope_id, &local_name);
                        let template_params = template_params_for_parent(node.parent(), self.source);
                        let class_id = self.symbols.new_class_id(&class_name);
                        let parents = collect_parent_types(self, node, scope_id);
                        let type_id = self.types.intern(TypeKind::Class(class_id));
                        let class_parent_scope = self
                            .register_template_param_scope(scope_id, &template_params)
                            .unwrap_or(scope_id);
                        let class_scope =
                            self.scopes.add_scope(Some(class_parent_scope), ScopeKind::Class);
                        self.symbols.set_class_scope(class_id, class_scope);
                        self.symbols.set_class_parents(class_id, parents.clone());
                        if !template_params.is_empty() {
                            self.class_template_params
                                .insert(class_id, template_params.clone());
                        }
                        self.attach_scope(node, class_scope);
                        let symbol_id = self.add_symbol(
                            scope_id,
                            symbol_name,
                            SymbolKind::Class {
                                class_id,
                                type_id,
                                parents,
                                is_struct: node.kind().contains("struct"),
                            },
                        );
                        self.scope_owner_symbols.insert(class_scope, symbol_id);
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
            "type_definition" | "alias_declaration" => {
                self.register_typedef_like(node, scope_id);
                return;
            }
            "using_declaration" | "using_directive" => {
                self.register_using_declaration(node, scope_id);
                return;
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
            "for_statement" | "for_range_loop" | "range_based_for_statement" => {
                let loop_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Block);
                self.attach_scope(node, loop_scope);
                self.walk_children(node, loop_scope, current_class);
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

    fn class_registration_names(
        &self,
        node: Node,
        scope_id: ScopeId,
        local_name: &str,
    ) -> (String, String) {
        if let Some(specialized_local_name) = self.explicit_specialization_local_name(node, local_name)
        {
            let qualified = self.qualified_name_in_scope(scope_id, &specialized_local_name);
            return (
                if qualified.is_empty() {
                    specialized_local_name.clone()
                } else {
                    qualified
                },
                specialized_local_name,
            );
        }

        (local_name.to_string(), local_name.to_string())
    }

    fn explicit_specialization_local_name(
        &self,
        node: Node,
        local_name: &str,
    ) -> Option<String> {
        let parent = node.parent()?;
        if parent.kind() != "template_declaration" {
            return None;
        }
        let parent_text = self.node_text(parent);
        let trimmed = parent_text.trim_start();
        if !trimmed.starts_with("template<>") && !trimmed.starts_with("template <>") {
            return None;
        }
        if local_name.contains('<') {
            return Some(local_name.to_string());
        }

        let node_text = self.node_text(node);
        let name_index = node_text.find(local_name)?;
        let after_name = node_text.get(name_index + local_name.len()..)?;
        let suffix = extract_template_suffix(after_name)?;
        Some(format!("{local_name}{suffix}"))
    }

    fn qualified_name_in_scope(&self, scope_id: ScopeId, local_name: &str) -> String {
        let mut segments = Vec::new();
        let mut current = Some(scope_id);
        while let Some(scope_id) = current {
            let Some(symbol_id) = self.scope_owner_symbols.get(&scope_id).copied() else {
                current = self.scopes.get(scope_id).and_then(|scope| scope.parent);
                continue;
            };
            let Some(symbol) = self.symbols.get(symbol_id) else {
                current = self.scopes.get(scope_id).and_then(|scope| scope.parent);
                continue;
            };
            if matches!(symbol.kind, SymbolKind::Namespace { .. }) {
                segments.push(symbol.name.clone());
            }
            current = self.scopes.get(scope_id).and_then(|scope| scope.parent);
        }
        segments.reverse();
        segments.push(local_name.to_string());
        segments.join("::")
    }

    fn register_template_param_scope(
        &mut self,
        parent_scope: ScopeId,
        params: &[TemplateScopeParam],
    ) -> Option<ScopeId> {
        if params.is_empty() {
            return None;
        }

        let template_scope = self.scopes.add_scope(Some(parent_scope), ScopeKind::Template);
        for param in params {
            match param {
                TemplateScopeParam::Type { name } => {
                    let type_id = self.types.intern(TypeKind::Dependent(name.clone()));
                    self.add_symbol(template_scope, name.clone(), SymbolKind::Typedef { type_id });
                }
                TemplateScopeParam::Value { name, declared_type } => {
                    let type_id = self.resolve_type_text(parent_scope, declared_type);
                    self.add_symbol(
                        template_scope,
                        name.clone(),
                        SymbolKind::Variable {
                            type_id,
                            storage: Storage::Parameter,
                        },
                    );
                }
            }
        }

        Some(template_scope)
    }

    fn materialize_template_instance_usage(&mut self, type_id: TypeId) {
        let Some(kind) = self.types.get(type_id).cloned() else {
            return;
        };

        match kind {
            TypeKind::Pointer { pointee, cv } => {
                let pointee = self.materialize_substituted_type(pointee, &HashMap::new());
                let _ = self.types.intern(TypeKind::Pointer { pointee, cv });
            }
            TypeKind::Reference { referent, kind } => {
                let referent = self.materialize_substituted_type(referent, &HashMap::new());
                let _ = self.types.intern(TypeKind::Reference { referent, kind });
            }
            TypeKind::Array { elem, size } => {
                let elem = self.materialize_substituted_type(elem, &HashMap::new());
                let _ = self.types.intern(TypeKind::Array { elem, size });
            }
            TypeKind::Function {
                return_t,
                params,
                min_arity,
                is_variadic,
                is_const_member,
            } => {
                let return_t = self.materialize_substituted_type(return_t, &HashMap::new());
                let params = params
                    .into_iter()
                    .map(|param| self.materialize_substituted_type(param, &HashMap::new()))
                    .collect::<Vec<_>>();
                let _ = self.types.intern(TypeKind::Function {
                    return_t,
                    params,
                    min_arity,
                    is_variadic,
                    is_const_member,
                });
            }
            TypeKind::Template { base, args } => {
                let args = args
                    .into_iter()
                    .map(|arg| match arg {
                        TemplateArg::Type(type_id) => {
                            TemplateArg::Type(self.materialize_substituted_type(
                                type_id,
                                &HashMap::new(),
                            ))
                        }
                        TemplateArg::Value(value) => TemplateArg::Value(value),
                    })
                    .collect::<Vec<_>>();
                let _ = self.types.intern(TypeKind::Template {
                    base: base.clone(),
                    args: args.clone(),
                });
                if let Some(type_id) =
                    self.resolve_existing_named_type(self.scopes.global, &base)
                    && let Some(TypeKind::Class(class_id)) = self.types.get(type_id)
                {
                    self.materialize_template_class_members(*class_id, &args);
                }
            }
            _ => {}
        }
    }

    fn materialize_template_class_members(
        &mut self,
        class_id: ClassId,
        args: &[TemplateArg],
    ) {
        let Some(params) = self.class_template_params.get(&class_id).cloned() else {
            return;
        };
        if params.is_empty() || args.is_empty() {
            return;
        }

        let mut bindings = HashMap::new();
        for (param, arg) in params.iter().zip(args.iter()) {
            bindings.insert(param.name().to_string(), arg.clone());
        }

        let Some(scope_id) = self.symbols.class_scope(class_id) else {
            return;
        };
        let symbol_ids = self
            .scopes
            .get(scope_id)
            .map(|scope| {
                scope
                    .symbols
                    .values()
                    .flat_map(|ids| ids.iter().copied())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for symbol_id in symbol_ids {
            self.materialize_symbol_with_bindings(symbol_id, &bindings);
        }
    }

    fn materialize_symbol_with_bindings(
        &mut self,
        symbol_id: super::symbol::SymbolId,
        bindings: &HashMap<String, TemplateArg>,
    ) {
        let Some(symbol) = self.symbols.get(symbol_id).cloned() else {
            return;
        };
        match symbol.kind {
            SymbolKind::Field { type_id, .. } | SymbolKind::Typedef { type_id } => {
                let _ = self.materialize_substituted_type(type_id, bindings);
            }
            SymbolKind::Method { type_id, .. } => {
                let _ = self.materialize_substituted_type(type_id, bindings);
            }
            _ => {}
        }
    }

    fn materialize_substituted_type(
        &mut self,
        type_id: TypeId,
        bindings: &HashMap<String, TemplateArg>,
    ) -> TypeId {
        let Some(kind) = self.types.get(type_id).cloned() else {
            return type_id;
        };

        match kind {
            TypeKind::Dependent(name) => match bindings.get(&name) {
                Some(TemplateArg::Type(type_id)) => {
                    self.materialize_template_instance_usage(*type_id);
                    *type_id
                }
                _ => type_id,
            },
            TypeKind::Pointer { pointee, cv } => {
                let pointee = self.materialize_substituted_type(pointee, bindings);
                self.types.intern(TypeKind::Pointer { pointee, cv })
            }
            TypeKind::Reference { referent, kind } => {
                let referent = self.materialize_substituted_type(referent, bindings);
                self.types.intern(TypeKind::Reference { referent, kind })
            }
            TypeKind::Array { elem, size } => {
                let elem = self.materialize_substituted_type(elem, bindings);
                self.types.intern(TypeKind::Array { elem, size })
            }
            TypeKind::Function {
                return_t,
                params,
                min_arity,
                is_variadic,
                is_const_member,
            } => {
                let return_t = self.materialize_substituted_type(return_t, bindings);
                let params = params
                    .into_iter()
                    .map(|param| self.materialize_substituted_type(param, bindings))
                    .collect::<Vec<_>>();
                self.types.intern(TypeKind::Function {
                    return_t,
                    params,
                    min_arity,
                    is_variadic,
                    is_const_member,
                })
            }
            TypeKind::Template { base, args } => {
                let args = args
                    .into_iter()
                    .map(|arg| match arg {
                        TemplateArg::Type(type_id) => {
                            TemplateArg::Type(self.materialize_substituted_type(type_id, bindings))
                        }
                        TemplateArg::Value(value) => {
                            bindings.get(&value).cloned().unwrap_or(TemplateArg::Value(value))
                        }
                    })
                    .collect::<Vec<_>>();
                let type_id = self.types.intern(TypeKind::Template {
                    base: base.clone(),
                    args: args.clone(),
                });
                if let Some(type_id) =
                    self.resolve_existing_named_type(self.scopes.global, &base)
                    && let Some(TypeKind::Class(class_id)) = self.types.get(type_id)
                {
                    self.materialize_template_class_members(*class_id, &args);
                }
                type_id
            }
            _ => type_id,
        }
    }

    fn walk_children(&mut self, node: Node, scope_id: ScopeId, current_class: Option<ClassId>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child, scope_id, current_class);
        }
    }

    fn ensure_namespace_scope(&mut self, scope_id: ScopeId, name: &str) -> ScopeId {
        let mut current_scope = scope_id;
        for segment in name.split("::").map(str::trim).filter(|segment| !segment.is_empty()) {
            current_scope = self.ensure_namespace_segment(current_scope, segment);
        }
        current_scope
    }

    fn ensure_namespace_segment(&mut self, scope_id: ScopeId, name: &str) -> ScopeId {
        if let Some(existing_scope) = self
            .scopes
            .get(scope_id)
            .and_then(|scope| scope.symbols.get(name))
            .and_then(|ids| {
                ids.iter().find_map(|id| {
                    let symbol = self.symbols.get(*id)?;
                    match symbol.kind {
                        SymbolKind::Namespace { children } => Some(children),
                        _ => None,
                    }
                })
            })
        {
            return existing_scope;
        }

        let child_scope = self.scopes.add_scope(Some(scope_id), ScopeKind::Namespace);
        let symbol_id =
            self.add_symbol(scope_id, name.to_string(), SymbolKind::Namespace { children: child_scope });
        self.scope_owner_symbols.insert(child_scope, symbol_id);
        child_scope
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

        let template_params = template_params_for_parent(node.parent(), self.source);
        let resolution_scope = self
            .register_template_param_scope(scope_id, &template_params)
            .unwrap_or(scope_id);

        let return_t = node
            .child_by_field_name("type")
            .map(|type_node| {
                let base = self.resolve_type_node(type_node, resolution_scope);
                let full_text = self.node_text(node).to_string();
                apply_declarator_wrappers(
                    &mut self.types,
                    base,
                    Some(declarator),
                    &full_text,
                )
            })
            .unwrap_or(self.types.unknown_t);
        self.materialize_template_instance_usage(return_t);
        let (params, min_arity) = function_parameter_info(self, declarator, resolution_scope);
        for param in &params {
            self.materialize_template_instance_usage(*param);
        }
        let fn_type = self.types.intern(TypeKind::Function {
            return_t,
            params,
            min_arity,
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
        let symbol_id = self.add_symbol(scope_id, name, kind);

        let fn_scope = self.scopes.add_scope(Some(resolution_scope), ScopeKind::Function);
        self.attach_scope(node, fn_scope);
        self.scope_owner_symbols.insert(fn_scope, symbol_id);
        self.register_parameters(declarator, fn_scope, current_class);

        if let Some(body) = find_descendant(node, "compound_statement") {
            self.walk(body, fn_scope, current_class);
        }
    }

    fn register_using_declaration(&mut self, node: Node, scope_id: ScopeId) {
        let text = self.node_text(node).trim().trim_end_matches(';').trim();
        if text.is_empty() {
            return;
        }

        let target = if let Some(rest) = text.strip_prefix("using namespace ") {
            format!("namespace {}", rest.trim())
        } else if let Some(rest) = text.strip_prefix("using ") {
            let candidate = rest.trim();
            if candidate.contains('=') {
                return;
            }
            candidate.to_string()
        } else {
            return;
        };

        let Some(scope) = self.scopes.get_mut(scope_id) else {
            return;
        };
        if target.is_empty() || scope.using_decls.iter().any(|existing| existing == &target) {
            return;
        }
        scope.using_decls.push(target);
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
            if child.kind() == "parameter_declaration"
                || child.kind() == "optional_parameter_declaration"
            {
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
                let base = self.resolve_type_node(type_node, scope_id);
                let full_text = self.node_text(node).to_string();
                apply_declarator_wrappers(
                    &mut self.types,
                    base,
                    node.child_by_field_name("declarator"),
                    &full_text,
                )
            })
            .unwrap_or(self.types.unknown_t);
        self.materialize_template_instance_usage(type_id);

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

    fn resolve_type_node(&mut self, node: Node, scope_id: ScopeId) -> TypeId {
        match node.kind() {
            "primitive_type" => {
                let text = self.node_text(node).to_string();
                builtin_type_by_name(&mut self.types, &text).unwrap_or(self.types.unknown_t)
            }
            "type_identifier" | "sized_type_specifier" => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                self.resolve_type_text(scope_id, &text)
            }
            "qualified_identifier" => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                self.resolve_type_text(scope_id, &text)
            }
            "template_type" => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                self.resolve_type_text(scope_id, &text)
            }
            _ => {
                let text = crate::parser::cpp::clean_type_string(self.node_text(node));
                self.resolve_type_text(scope_id, &text)
            }
        }
    }

    fn resolve_template_type_text(&mut self, scope_id: ScopeId, text: &str) -> TypeId {
        let Some(name_end) = text.find('<') else {
            return self.resolve_named_type(scope_id, text);
        };
        let base = text[..name_end].trim();
        let Some(inner) = text[name_end + 1..].strip_suffix('>') else {
            return self.resolve_named_type(scope_id, text);
        };
        if base.is_empty() {
            return self.resolve_named_type(scope_id, text);
        }

        let mut args = Vec::new();
        for arg in split_template_args(inner) {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                continue;
            }
            if looks_like_type_arg(trimmed) {
                args.push(TemplateArg::Type(self.resolve_type_text(scope_id, trimmed)));
            } else if let Some(type_id) = self.resolve_existing_named_type(scope_id, trimmed) {
                args.push(TemplateArg::Type(type_id));
            } else {
                args.push(TemplateArg::Value(trimmed.to_string()));
            }
        }

        self.types.intern(TypeKind::Template {
            base: base.to_string(),
            args,
        })
    }

    fn resolve_named_type(&mut self, scope_id: ScopeId, text: &str) -> TypeId {
        if let Some(type_id) = self.resolve_existing_named_type(scope_id, text) {
            return type_id;
        }
        let class_id = self.symbols.new_class_id(text);
        self.types.intern(TypeKind::Class(class_id))
    }

    fn resolve_existing_named_type(&mut self, scope_id: ScopeId, text: &str) -> Option<TypeId> {
        if let Some(builtin) = builtin_type_by_name(&mut self.types, text) {
            return Some(builtin);
        }
        if let Some(type_id) = self.lookup_typedef_type(scope_id, text) {
            return Some(type_id);
        }

        if text.contains("::") {
            return self.lookup_qualified_named_type(scope_id, text);
        }

        let mut current = Some(scope_id);
        while let Some(scope_id) = current {
            let scope = self.scopes.get(scope_id)?;
            if let Some(ids) = scope.symbols.get(text) {
                for symbol_id in ids {
                    if let Some(type_id) = self.type_id_for_symbol(*symbol_id) {
                        return Some(type_id);
                    }
                }
            }
            current = scope.parent;
        }

        None
    }

    fn lookup_qualified_named_type(&self, scope_id: ScopeId, text: &str) -> Option<TypeId> {
        let segments = text
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let (first, rest) = segments.split_first()?;

        let mut scope_cursor = Some(scope_id);
        while let Some(candidate_scope) = scope_cursor {
            let Some(ids) = self.lookup_scope_symbol_ids(candidate_scope, first) else {
                scope_cursor = self.scopes.get(candidate_scope).and_then(|scope| scope.parent);
                continue;
            };

            if let Some(type_id) = self.resolve_qualified_symbol_chain(ids, rest) {
                return Some(type_id);
            }

            scope_cursor = self.scopes.get(candidate_scope).and_then(|scope| scope.parent);
        }

        self.lookup_scope_symbol_ids(self.scopes.global, first)
            .and_then(|ids| self.resolve_qualified_symbol_chain(ids, rest))
    }

    fn lookup_scope_symbol_ids(&self, scope_id: ScopeId, name: &str) -> Option<Vec<super::symbol::SymbolId>> {
        self.scopes
            .get(scope_id)
            .and_then(|scope| scope.symbols.get(name).cloned())
    }

    fn resolve_qualified_symbol_chain(
        &self,
        ids: Vec<super::symbol::SymbolId>,
        rest: &[&str],
    ) -> Option<TypeId> {
        if rest.is_empty() {
            return ids.into_iter().find_map(|symbol_id| self.type_id_for_symbol(symbol_id));
        }

        let segment = rest[0];
        let mut next_ids = Vec::new();
        for symbol_id in ids {
            let Some(symbol) = self.symbols.get(symbol_id) else {
                continue;
            };
            let child_scope = match symbol.kind {
                SymbolKind::Namespace { children } => Some(children),
                SymbolKind::Class { class_id, .. } => self.symbols.class_scope(class_id),
                _ => None,
            };
            let Some(child_scope) = child_scope else {
                continue;
            };
            if let Some(child_ids) = self.lookup_scope_symbol_ids(child_scope, segment) {
                next_ids.extend(child_ids);
            }
        }

        (!next_ids.is_empty())
            .then(|| self.resolve_qualified_symbol_chain(next_ids, &rest[1..]))
            .flatten()
    }

    fn type_id_for_symbol(&self, symbol_id: super::symbol::SymbolId) -> Option<TypeId> {
        let symbol = self.symbols.get(symbol_id)?;
        match symbol.kind {
            SymbolKind::Class { type_id, .. }
            | SymbolKind::Enum { type_id, .. }
            | SymbolKind::Typedef { type_id } => Some(type_id),
            _ => None,
        }
    }

    fn lookup_typedef_type(&self, scope_id: ScopeId, name: &str) -> Option<TypeId> {
        let mut current = Some(scope_id);
        while let Some(scope_id) = current {
            let scope = self.scopes.get(scope_id)?;
            if let Some(ids) = scope.symbols.get(name) {
                for symbol_id in ids {
                    let symbol = self.symbols.get(*symbol_id)?;
                    if let SymbolKind::Typedef { type_id } = symbol.kind {
                        return Some(type_id);
                    }
                }
            }
            current = scope.parent;
        }
        None
    }

    fn register_typedef_like(&mut self, node: Node, scope_id: ScopeId) {
        let Some((name, aliased)) = self.typedef_like_parts(node, scope_id) else {
            return;
        };
        if name.is_empty() {
            return;
        }

        let type_id = self.types.intern(TypeKind::Typedef {
            name: name.clone(),
            aliased,
        });
        self.add_symbol(scope_id, name, SymbolKind::Typedef { type_id });
    }

    fn typedef_like_parts(&mut self, node: Node, scope_id: ScopeId) -> Option<(String, TypeId)> {
        if node.kind() == "alias_declaration" {
            return self.alias_declaration_parts(node, scope_id);
        }

        let declarator = node.child_by_field_name("declarator")?;
        let name_node = find_name_node(declarator)?;
        let name = self.node_text(name_node).trim().to_string();
        let type_node = node.child_by_field_name("type")?;
        let base = self.resolve_type_node(type_node, scope_id);
        let full_text = self.node_text(node).to_string();
        let type_id =
            apply_declarator_wrappers(&mut self.types, base, Some(declarator), &full_text);
        Some((name, type_id))
    }

    fn alias_declaration_parts(&mut self, node: Node, scope_id: ScopeId) -> Option<(String, TypeId)> {
        let text = self.node_text(node).trim().trim_end_matches(';').trim();
        let rest = text.strip_prefix("using ")?.trim();
        let (name, target) = rest.split_once('=')?;
        let name = name.trim().to_string();
        let target = target.trim().to_string();
        let type_id = self.resolve_type_text(scope_id, &target);
        Some((name, type_id))
    }

    fn resolve_type_text(&mut self, scope_id: ScopeId, raw: &str) -> TypeId {
        let clean = crate::parser::cpp::clean_type_string(raw);
        if clean.is_empty() {
            return self.types.unknown_t;
        }
        if let Some(inner) = clean.strip_suffix("[]") {
            let elem = self.resolve_type_text(scope_id, inner.trim_end());
            return self.types.intern(TypeKind::Array { elem, size: None });
        }
        if let Some(inner) = clean.strip_suffix('&') {
            let referent = self.resolve_type_text(scope_id, inner.trim_end());
            return self.types.intern(TypeKind::Reference {
                referent,
                kind: RefKind::LValue,
            });
        }
        if let Some(inner) = clean.strip_suffix('*') {
            let pointee = self.resolve_type_text(scope_id, inner.trim_end());
            return self.types.intern(TypeKind::Pointer {
                pointee,
                cv: CvQual::default(),
            });
        }
        if clean.contains('<') && clean.ends_with('>') {
            return self.resolve_template_type_text(scope_id, &clean);
        }
        self.resolve_named_type(scope_id, &clean)
    }

    fn add_symbol(
        &mut self,
        scope_id: ScopeId,
        name: String,
        kind: SymbolKind,
    ) -> super::symbol::SymbolId {
        let symbol_id = self.symbols.add_symbol(name.clone(), scope_id, kind);
        if let Some(scope) = self.scopes.get_mut(scope_id) {
            scope.symbols.entry(name).or_default().push(symbol_id);
        }
        symbol_id
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

fn function_parameter_info(
    builder: &mut SemaBuilder<'_>,
    declarator: Node,
    scope_id: ScopeId,
) -> (Vec<TypeId>, usize) {
    let Some(params) = find_descendant(declarator, "parameter_list") else {
        return (Vec::new(), 0);
    };
    let mut out = Vec::new();
    let mut min_arity = 0usize;
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            let type_id = child
                .child_by_field_name("type")
                .map(|type_node| {
                    let base = builder.resolve_type_node(type_node, scope_id);
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
            if find_descendant(child, "default_argument").is_none() {
                min_arity += 1;
            }
        }
    }
    (out, min_arity)
}

fn apply_declarator_wrappers(
    arena: &mut TypeArena,
    mut current: TypeId,
    declarator: Option<Node>,
    _full_text: &str,
) -> TypeId {
    let mut cursor = declarator;
    while let Some(node) = cursor {
        current = match node.kind() {
            "pointer_declarator" => {
                arena.intern(TypeKind::Pointer {
                    pointee: current,
                    cv: CvQual::default(),
                })
            }
            "reference_declarator" => {
                arena.intern(TypeKind::Reference {
                    referent: current,
                    kind: RefKind::LValue,
                })
            }
            "array_declarator" => {
                arena.intern(TypeKind::Array {
                    elem: current,
                    size: None,
                })
            }
            _ => current,
        };
        cursor = next_declarator_node(node);
    }

    current
}

fn class_like_name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
        .or_else(|| find_direct_child_by_kind(node, &["type_identifier", "identifier"]))
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

fn looks_like_type_arg(text: &str) -> bool {
    text.contains("::")
        || text.contains('<')
        || matches!(
            text,
            "void"
                | "bool"
                | "char"
                | "ANSICHAR"
                | "TCHAR"
                | "int"
                | "int32"
                | "uint32"
                | "float"
                | "double"
        )
}

fn template_params_for_parent(
    parent: Option<Node>,
    source: &str,
) -> Vec<TemplateScopeParam> {
    let Some(parent) = parent else {
        return Vec::new();
    };
    if parent.kind() != "template_declaration" {
        return Vec::new();
    }

    let Some(params_node) = parent.child_by_field_name("parameters") else {
        return Vec::new();
    };

    let mut params = Vec::new();
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        match child.kind() {
            "type_parameter_declaration" | "optional_type_parameter_declaration" => {
                if let Some(name_node) = find_descendant(child, "type_identifier")
                    .or_else(|| find_descendant(child, "identifier"))
                {
                    let name = builder_node_text(name_node, source).trim().to_string();
                    if !name.is_empty() {
                        params.push(TemplateScopeParam::Type { name });
                    }
                }
            }
            "parameter_declaration" => {
                let declared_type = child
                    .child_by_field_name("type")
                    .map(|node| {
                        crate::parser::cpp::clean_type_string(builder_node_text(node, source))
                    })
                    .unwrap_or_default();
                let name = child
                    .child_by_field_name("declarator")
                    .and_then(find_name_node)
                    .map(|node| builder_node_text(node, source).trim().to_string())
                    .unwrap_or_default();
                if !name.is_empty() && !declared_type.is_empty() {
                    params.push(TemplateScopeParam::Value { name, declared_type });
                }
            }
            _ => {}
        }
    }

    params
}

fn builder_node_text<'a>(node: Node, source: &'a str) -> &'a str {
    let range = node.byte_range();
    if range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
    {
        &source[range.start..range.end]
    } else {
        ""
    }
}

fn extract_template_suffix(text: &str) -> Option<String> {
    let start = text.find('<')?;
    let mut depth = 0usize;
    let mut suffix = String::new();
    for ch in text[start..].chars() {
        suffix.push(ch);
        match ch {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(suffix);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_parent_types(
    builder: &mut SemaBuilder<'_>,
    node: Node,
    scope_id: ScopeId,
) -> Vec<TypeId> {
    let Some(base_clause) = find_descendant(node, "base_class_clause") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = base_clause.walk();
    for child in base_clause.children(&mut cursor) {
        if matches!(
            child.kind(),
            "type_identifier" | "qualified_identifier" | "template_type"
        ) {
            let type_id = builder.resolve_type_node(child, scope_id);
            out.push(type_id);
        }
    }
    out
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
        | "bitfield_clause" => next_declarator_node(node).and_then(find_name_node),
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

fn next_declarator_node(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "identifier"
                    | "field_identifier"
                    | "type_identifier"
                    | "qualified_identifier"
                    | "function_declarator"
                    | "pointer_declarator"
                    | "reference_declarator"
                    | "array_declarator"
                    | "parenthesized_declarator"
                    | "init_declarator"
                    | "bitfield_clause"
            )
        })
    })
}
