use std::collections::HashMap;

use tree_sitter::Node;

use super::symbol::SourceRef;
use super::types::{CvQual, RefKind, TemplateArg, TypeId, TypeKind};
use super::SemaContext;

#[derive(Clone, Debug)]
pub enum TemplateParam {
    TypeParam {
        name: String,
        default_arg: Option<ExplicitTemplateArg>,
    },
    NonTypeParam {
        name: String,
        declared_type: String,
        default_arg: Option<ExplicitTemplateArg>,
    },
}

#[derive(Clone, Debug)]
pub struct TemplateDecl {
    pub name: String,
    pub params: Vec<TemplateParam>,
    pub kind: TemplateDeclKind,
    pub source: SourceRef,
    pub end_source: SourceRef,
    pub is_sfinae_guarded: bool,
    pub sfinae_always_rejects: bool,
    pub is_explicit_specialization: bool,
    pub specialization_args: Vec<ExplicitTemplateArg>,
    pub signature_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstraintState {
    None,
    Guarded,
    AlwaysReject,
}

#[derive(Clone, Debug)]
pub enum TemplateDeclKind {
    Function {
        function_params: Vec<TypePattern>,
        return_pattern: TypePattern,
    },
    Class {
        constructors: Vec<Vec<TypePattern>>,
    },
}

#[derive(Clone, Debug)]
pub enum TypePattern {
    TemplateParam(String),
    NonTypeParam(String),
    Pointer(Box<TypePattern>),
    Reference(Box<TypePattern>),
    TemplateInstance { base: String, args: Vec<TypePattern> },
    Concrete(String),
    Value(String),
}

#[derive(Clone, Debug)]
pub enum ExplicitTemplateArg {
    Type(String),
    Value(String),
}

#[derive(Clone, Debug)]
pub enum TemplateCallFailure {
    DeductionFail,
    ExplicitArityMismatch,
    NonTypeArgMismatch,
    SfinaeRejected,
}

#[derive(Clone, Debug)]
pub struct TemplateCallAnalysis {
    pub failure: TemplateCallFailure,
}

#[derive(Clone, Debug, Default)]
pub struct TemplateIndex {
    decls_by_name: HashMap<String, Vec<TemplateDecl>>,
    specialization_keys: HashMap<String, Vec<SourceRange>>,
}

#[derive(Clone, Debug)]
pub struct SourceRange {
    pub start: SourceRef,
    pub end: SourceRef,
}

#[derive(Clone, Debug)]
enum TemplateBinding {
    Text(String),
    Type(TypeId),
    Value(String),
}

impl TemplateIndex {
    pub fn collect(root: Node, sema_ctx: &SemaContext) -> Self {
        let mut index = Self::default();
        collect_template_decls(root, sema_ctx, &mut index);
        index
    }

    pub fn analyze_call(
        &self,
        call: Node,
        sema_ctx: &SemaContext,
    ) -> Option<TemplateCallAnalysis> {
        let callee = call.child_by_field_name("function").or_else(|| call.child(0))?;
        let name = template_callee_name(callee, sema_ctx)?;
        let decls = self.decls_by_name.get(&name)?;
        let explicit_args = explicit_template_args(callee, sema_ctx);
        let arg_types = call_argument_types(call, sema_ctx);
        let has_explicit_args = has_template_argument_list(callee);

        let mut saw_arity = false;
        let mut saw_non_type_mismatch = false;
        let mut saw_sfinae = false;
        let mut saw_deduction_fail = false;

        for decl in ordered_template_decls(decls) {
            match analyze_decl_call(decl, &explicit_args, &arg_types, sema_ctx) {
                None => return None,
                Some(TemplateCallFailure::ExplicitArityMismatch) => saw_arity = true,
                Some(TemplateCallFailure::NonTypeArgMismatch) => saw_non_type_mismatch = true,
                Some(TemplateCallFailure::SfinaeRejected) => saw_sfinae = true,
                Some(TemplateCallFailure::DeductionFail) => saw_deduction_fail = true,
            }
        }

        let failure = if saw_arity {
            TemplateCallFailure::ExplicitArityMismatch
        } else if saw_non_type_mismatch {
            TemplateCallFailure::NonTypeArgMismatch
        } else if saw_sfinae {
            TemplateCallFailure::SfinaeRejected
        } else if saw_deduction_fail || has_explicit_args || !arg_types.is_empty() {
            TemplateCallFailure::DeductionFail
        } else {
            return None;
        };

        Some(TemplateCallAnalysis { failure })
    }

    pub fn infer_call_return_type(&self, call: Node, sema_ctx: &SemaContext) -> Option<TypeId> {
        let callee = call.child_by_field_name("function").or_else(|| call.child(0))?;
        let name = template_callee_name(callee, sema_ctx)?;
        let decls = self.decls_by_name.get(&name)?;
        let explicit_args = explicit_template_args(callee, sema_ctx);
        let arg_types = call_argument_types(call, sema_ctx);

        for decl in ordered_template_decls(decls) {
            match &decl.kind {
                TemplateDeclKind::Function { return_pattern, .. } => {
                    if decl.is_explicit_specialization {
                        if analyze_explicit_specialization_call(
                            decl,
                            &explicit_args,
                            &arg_types,
                            sema_ctx,
                        )
                        .is_none()
                            && let Some(type_id) = instantiate_return_pattern(
                                return_pattern,
                                &HashMap::new(),
                                sema_ctx,
                            )
                        {
                            return Some(type_id);
                        }
                        continue;
                    }
                    let Ok(bindings) = bind_decl_call(decl, &explicit_args, &arg_types, sema_ctx)
                    else {
                        continue;
                    };
                    if let Some(type_id) =
                        instantiate_return_pattern(return_pattern, &bindings, sema_ctx)
                    {
                        return Some(type_id);
                    }
                }
                TemplateDeclKind::Class { .. } => {
                    if decl.is_explicit_specialization {
                        if analyze_explicit_specialization_call(
                            decl,
                            &explicit_args,
                            &arg_types,
                            sema_ctx,
                        )
                        .is_none()
                            && let Some(type_id) =
                                instantiate_specialization_decl_type(decl, sema_ctx)
                        {
                            return Some(type_id);
                        }
                        continue;
                    }
                    let Ok(bindings) = bind_decl_call(decl, &explicit_args, &arg_types, sema_ctx)
                    else {
                        continue;
                    };
                    if let Some(type_id) = instantiate_decl_type(decl, &bindings, sema_ctx) {
                        return Some(type_id);
                    }
                }
            }
        }
        None
    }

    pub fn analyze_template_type(
        &self,
        template_type: Node,
        sema_ctx: &SemaContext,
    ) -> Option<TemplateCallAnalysis> {
        if template_type.kind() != "template_type" {
            return None;
        }
        let name = template_type_name(template_type, sema_ctx)?;
        if name.is_empty() {
            return None;
        }
        let decls = self.decls_by_name.get(name)?;
        let explicit_args = explicit_template_args_from_type(template_type, sema_ctx);

        let mut saw_arity = false;
        let mut saw_non_type_mismatch = false;
        let mut saw_sfinae = false;

        for decl in ordered_template_decls(decls) {
            if !matches!(decl.kind, TemplateDeclKind::Class { .. }) {
                continue;
            }
            match analyze_class_template_usage(decl, &explicit_args) {
                None => return None,
                Some(TemplateCallFailure::ExplicitArityMismatch) => saw_arity = true,
                Some(TemplateCallFailure::NonTypeArgMismatch) => saw_non_type_mismatch = true,
                Some(TemplateCallFailure::SfinaeRejected) => saw_sfinae = true,
                Some(_) => {}
            }
        }

        let failure = if saw_arity {
            TemplateCallFailure::ExplicitArityMismatch
        } else if saw_non_type_mismatch {
            TemplateCallFailure::NonTypeArgMismatch
        } else if saw_sfinae {
            TemplateCallFailure::SfinaeRejected
        } else {
            return None;
        };

        Some(TemplateCallAnalysis { failure })
    }

    pub fn specialization_conflicts(&self) -> Vec<String> {
        self.specialization_keys
            .iter()
            .filter_map(|(key, count)| (count.len() > 1).then_some(key.clone()))
            .collect()
    }

    pub fn specialization_conflict_entries(&self) -> Vec<(String, SourceRange)> {
        self.specialization_keys
            .iter()
            .filter(|(_, ranges)| ranges.len() > 1)
            .flat_map(|(key, ranges)| {
                ranges
                    .iter()
                    .cloned()
                    .map(|range| (key.clone(), range))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn collect_template_decls(node: Node, sema_ctx: &SemaContext, index: &mut TemplateIndex) {
    if node.kind() == "template_declaration" {
        if let Some(decl) = parse_template_decl(node, sema_ctx) {
            if decl.is_explicit_specialization {
                index
                    .specialization_keys
                    .entry(decl.signature_key.clone())
                    .or_default()
                    .push(SourceRange {
                        start: decl.source.clone(),
                        end: decl.end_source.clone(),
                    });
            }
            for key in template_lookup_keys(&decl.name) {
                index
                    .decls_by_name
                    .entry(key)
                    .or_default()
                    .push(decl.clone());
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_template_decls(child, sema_ctx, index);
    }
}

fn parse_template_decl(node: Node, sema_ctx: &SemaContext) -> Option<TemplateDecl> {
    let source = sema_ctx.source()?;
    let template_text = node_text(node, source)?;
    let params_node = node.child_by_field_name("parameters");
    let params = parse_template_params(params_node, source);
    let param_kinds = build_template_param_kind_map(&params);

    let (name, kind, signature_key) = if let Some(class_node) = find_template_class_node(node) {
        let name_node = class_like_name_node(class_node)?;
        let name = qualify_template_decl_name(node, node_text(name_node, source)?.trim(), source);
        if name.is_empty() {
            return None;
        }
        let constructors =
            parse_class_template_constructors(
                class_node,
                source,
                &param_kinds,
                short_name(&name),
            );
        (
            name.clone(),
            TemplateDeclKind::Class { constructors },
            normalize_class_signature_key(class_node, source, &name),
        )
    } else {
        let function_node = find_descendant(node, "function_definition")
            .or_else(|| find_descendant(node, "declaration"))?;
        let declarator = function_node
            .child_by_field_name("declarator")
            .or_else(|| find_descendant(function_node, "function_declarator"))?;
        let name_node = find_name_node(declarator)?;
        let name = qualify_template_decl_name(node, node_text(name_node, source)?.trim(), source);
        if name.is_empty() {
            return None;
        }
        let function_params =
            parse_function_param_patterns(function_node, source, &param_kinds);
        let return_pattern = function_node
            .child_by_field_name("type")
            .map(|node| {
                let pattern = type_pattern_from_node(node, source, &param_kinds);
                apply_pattern_declarator_wrappers(pattern, Some(declarator))
            })
            .unwrap_or(TypePattern::Concrete("void".to_string()));
        (
            name.clone(),
            TemplateDeclKind::Function {
                function_params,
                return_pattern,
            },
            normalize_signature_key(function_node, source, &name),
        )
    };

    let constraint_state = template_constraint_state(node, source);

    Some(TemplateDecl {
        name,
        params,
        kind,
        source: SourceRef {
            line: node.start_position().row as u32,
            column: node.start_position().column as u32,
        },
        end_source: SourceRef {
            line: node.end_position().row as u32,
            column: node.end_position().column as u32,
        },
        is_sfinae_guarded: !matches!(constraint_state, ConstraintState::None),
        sfinae_always_rejects: matches!(constraint_state, ConstraintState::AlwaysReject),
        is_explicit_specialization: template_text.trim_start().starts_with("template<>")
            || template_text.trim_start().starts_with("template <>"),
        specialization_args: explicit_specialization_args(node, sema_ctx),
        signature_key,
    })
}

fn template_constraint_state(node: Node, source: &str) -> ConstraintState {
    let mut state = ConstraintState::None;

    if let Some(requires_clause) = find_descendant(node, "requires_clause") {
        let normalized = node_text(requires_clause, source)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let predicate = normalized
            .strip_prefix("requires ")
            .unwrap_or(normalized.as_str())
            .trim();
        return if predicate == "false" {
            ConstraintState::AlwaysReject
        } else {
            ConstraintState::Guarded
        };
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_descendant(child, "template_type")
            && let Some(found_state) = enable_if_constraint_state(found, source)
        {
            if matches!(found_state, ConstraintState::AlwaysReject) {
                return ConstraintState::AlwaysReject;
            }
            state = ConstraintState::Guarded;
        }
    }

    state
}

fn enable_if_constraint_state(node: Node, source: &str) -> Option<ConstraintState> {
    let base = template_type_base_name(node, source)?;
    if !is_enable_if_base(&base) {
        return None;
    }
    let args = template_type_args(node, source);
    let condition = args.first()?.trim();
    Some(if normalize_template_value(condition) == "false" {
        ConstraintState::AlwaysReject
    } else {
        ConstraintState::Guarded
    })
}

fn parse_template_params(node: Option<Node>, source: &str) -> Vec<TemplateParam> {
    let Some(node) = node else {
        return Vec::new();
    };

    let mut params = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_parameter_declaration" | "optional_type_parameter_declaration" => {
                if let Some(name_node) = find_descendant(child, "type_identifier")
                    .or_else(|| find_descendant(child, "identifier"))
                {
                    let name = node_text(name_node, source).unwrap_or("").trim().to_string();
                    if !name.is_empty() {
                        params.push(TemplateParam::TypeParam {
                            name,
                            default_arg: parse_type_param_default(child, source),
                        });
                    }
                }
            }
            "parameter_declaration" => {
                let declared_type = child
                    .child_by_field_name("type")
                    .and_then(|node| node_text(node, source))
                    .map(crate::parser::cpp::clean_type_string)
                    .unwrap_or_default();
                let name = child
                    .child_by_field_name("declarator")
                    .and_then(find_name_node)
                    .and_then(|node| node_text(node, source))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !name.is_empty() && !declared_type.is_empty() {
                    params.push(TemplateParam::NonTypeParam {
                        name,
                        declared_type,
                        default_arg: parse_non_type_param_default(child, source),
                    });
                }
            }
            _ => {}
        }
    }

    params
}

fn parse_function_param_patterns(
    function_node: Node,
    source: &str,
    template_param_kinds: &HashMap<String, bool>,
) -> Vec<TypePattern> {
    let Some(declarator) = function_node
        .child_by_field_name("declarator")
        .or_else(|| find_descendant(function_node, "function_declarator"))
    else {
        return Vec::new();
    };
    let Some(param_list) = find_descendant(declarator, "parameter_list") else {
        return Vec::new();
    };

    let mut patterns = Vec::new();
    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() != "parameter_declaration" {
            continue;
        }
        if let Some(pattern) = parameter_pattern(child, source, template_param_kinds) {
            patterns.push(pattern);
        }
    }
    patterns
}

fn parameter_pattern(
    node: Node,
    source: &str,
    template_param_kinds: &HashMap<String, bool>,
) -> Option<TypePattern> {
    let type_node = node.child_by_field_name("type")?;
    let pattern = type_pattern_from_node(type_node, source, template_param_kinds);
    Some(apply_pattern_declarator_wrappers(
        pattern,
        node.child_by_field_name("declarator"),
    ))
}

fn type_pattern_from_node(
    node: Node,
    source: &str,
    template_param_kinds: &HashMap<String, bool>,
) -> TypePattern {
    match node.kind() {
        "type_descriptor" => node
            .named_child(0)
            .map(|child| type_pattern_from_node(child, source, template_param_kinds))
            .unwrap_or_else(|| {
                TypePattern::Concrete(crate::parser::cpp::clean_type_string(
                    node_text(node, source).unwrap_or(""),
                ))
            }),
        "qualified_identifier" | "qualified_unreal_type_identifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let mut pattern = type_pattern_from_node(name_node, source, template_param_kinds);
                if let TypePattern::TemplateInstance { base, .. } = &mut pattern
                    && let Some(scope_node) = node.child_by_field_name("scope")
                {
                    let scope = crate::parser::cpp::clean_type_string(
                        node_text(scope_node, source).unwrap_or(""),
                    );
                    if !scope.is_empty() {
                        *base = format!("{scope}::{base}");
                    }
                }
                return pattern;
            }

            TypePattern::Concrete(crate::parser::cpp::clean_type_string(
                node_text(node, source).unwrap_or(""),
            ))
        }
        "template_type" => {
            let base = node
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source))
                .map(crate::parser::cpp::clean_type_string)
                .unwrap_or_default();
            let mut args = Vec::new();
            if let Some(arg_list) = node.child_by_field_name("arguments") {
                let mut cursor = arg_list.walk();
                for child in arg_list.children(&mut cursor) {
                    match child.kind() {
                        "type_descriptor"
                        | "type_identifier"
                        | "primitive_type"
                        | "qualified_identifier"
                        | "template_type" => {
                            args.push(type_pattern_from_node(child, source, template_param_kinds));
                        }
                        "identifier" | "field_identifier" => {
                            let text = node_text(child, source).unwrap_or("").trim().to_string();
                            if let Some(is_type) = template_param_kinds.get(&text) {
                                args.push(if *is_type {
                                    TypePattern::TemplateParam(text)
                                } else {
                                    TypePattern::NonTypeParam(text)
                                });
                            } else if !text.is_empty() {
                                args.push(TypePattern::Value(text));
                            }
                        }
                        "number_literal" | "char_literal" | "true" | "false" => {
                            let text = node_text(child, source).unwrap_or("").trim().to_string();
                            if !text.is_empty() {
                                args.push(TypePattern::Value(text));
                            }
                        }
                        _ => {}
                    }
                }
            }
            TypePattern::TemplateInstance { base, args }
        }
        _ => {
            let text = crate::parser::cpp::clean_type_string(node_text(node, source).unwrap_or(""));
            if let Some(is_type) = template_param_kinds.get(&text) {
                if *is_type {
                    TypePattern::TemplateParam(text)
                } else {
                    TypePattern::NonTypeParam(text)
                }
            } else {
                TypePattern::Concrete(text)
            }
        }
    }
}

fn apply_pattern_declarator_wrappers(
    mut pattern: TypePattern,
    declarator: Option<Node>,
) -> TypePattern {
    let mut cursor = declarator;
    while let Some(node) = cursor {
        pattern = match node.kind() {
            "pointer_declarator" => TypePattern::Pointer(Box::new(pattern)),
            "reference_declarator" => TypePattern::Reference(Box::new(pattern)),
            _ => pattern,
        };
        cursor = next_declarator_node(node);
    }
    pattern
}

fn analyze_decl_call(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
    arg_types: &[TypeId],
    sema_ctx: &SemaContext,
) -> Option<TemplateCallFailure> {
    if decl.is_explicit_specialization {
        return analyze_explicit_specialization_call(decl, explicit_args, arg_types, sema_ctx);
    }
    bind_decl_call(decl, explicit_args, arg_types, sema_ctx).err()
}

fn bind_decl_call(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
    arg_types: &[TypeId],
    sema_ctx: &SemaContext,
) -> Result<HashMap<String, TemplateBinding>, TemplateCallFailure> {
    let mut bindings = HashMap::<String, TemplateBinding>::new();
    for (index, explicit_arg) in explicit_args.iter().enumerate() {
        let Some(param) = decl.params.get(index) else {
            return Err(TemplateCallFailure::ExplicitArityMismatch);
        };
        bind_explicit_template_arg(param, explicit_arg, sema_ctx, &mut bindings)?;
    }

    match &decl.kind {
        TemplateDeclKind::Function { function_params, .. } => {
            if arg_types.len() != function_params.len() {
                return Err(TemplateCallFailure::DeductionFail);
            }
            for (pattern, arg_type) in function_params.iter().zip(arg_types.iter()) {
                if !match_type_pattern(pattern, *arg_type, sema_ctx, &mut bindings) {
                    return Err(if decl.is_sfinae_guarded {
                        TemplateCallFailure::SfinaeRejected
                    } else {
                        TemplateCallFailure::DeductionFail
                    });
                }
            }
        }
        TemplateDeclKind::Class { constructors } => {
            if !bind_class_template_ctor(decl, constructors, arg_types, sema_ctx, &mut bindings) {
                return Err(if decl.is_sfinae_guarded {
                    TemplateCallFailure::SfinaeRejected
                } else {
                    TemplateCallFailure::DeductionFail
                });
            }
        }
    }

    for param in decl.params.iter().skip(explicit_args.len()) {
        bind_default_template_arg(param, sema_ctx, &mut bindings)?;
    }

    for param in &decl.params {
        match param {
            TemplateParam::TypeParam { name, .. } => {
                if !bindings.contains_key(name) {
                    return Err(if decl.is_sfinae_guarded {
                        TemplateCallFailure::SfinaeRejected
                    } else {
                        TemplateCallFailure::DeductionFail
                    });
                }
            }
            TemplateParam::NonTypeParam {
                name,
                declared_type,
                ..
            } => {
                let Some(binding) = bindings.get(name) else {
                    return Err(TemplateCallFailure::DeductionFail);
                };
                if !matches_non_type_binding(binding, declared_type) {
                    return Err(TemplateCallFailure::NonTypeArgMismatch);
                }
            }
        }
    }

    if decl.sfinae_always_rejects {
        return Err(TemplateCallFailure::SfinaeRejected);
    }

    Ok(bindings)
}

fn analyze_class_template_usage(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
) -> Option<TemplateCallFailure> {
    if !matches!(decl.kind, TemplateDeclKind::Class { .. }) {
        return None;
    }
    if decl.is_explicit_specialization {
        let failure =
            explicit_specialization_args_match(decl, explicit_args, None).err()?;
        return Some(failure);
    }
    let explicit_args = match apply_default_template_args(&decl.params, explicit_args, false) {
        Ok(args) => args,
        Err(failure) => return Some(failure),
    };
    for (param, arg) in decl.params.iter().zip(explicit_args.iter()) {
        match (param, arg) {
            (TemplateParam::TypeParam { .. }, ExplicitTemplateArg::Type(_)) => {}
            (
                TemplateParam::NonTypeParam { declared_type, .. },
                ExplicitTemplateArg::Value(value),
            ) => {
                if !matches_non_type_value(value, declared_type) {
                    return Some(TemplateCallFailure::NonTypeArgMismatch);
                }
            }
            (TemplateParam::NonTypeParam { .. }, ExplicitTemplateArg::Type(_)) => {
                return Some(TemplateCallFailure::NonTypeArgMismatch);
            }
            (TemplateParam::TypeParam { .. }, ExplicitTemplateArg::Value(_)) => {
                return Some(TemplateCallFailure::DeductionFail);
            }
        }
    }
    if decl.sfinae_always_rejects {
        return Some(TemplateCallFailure::SfinaeRejected);
    }
    None
}

fn analyze_explicit_specialization_call(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
    arg_types: &[TypeId],
    sema_ctx: &SemaContext,
) -> Option<TemplateCallFailure> {
    if let Err(failure) = explicit_specialization_args_match(decl, explicit_args, Some(sema_ctx)) {
        return Some(failure);
    }

    match &decl.kind {
        TemplateDeclKind::Function { function_params, .. } => {
            if arg_types.len() != function_params.len() {
                return Some(TemplateCallFailure::DeductionFail);
            }
            let mut bindings = HashMap::new();
            for (pattern, arg_type) in function_params.iter().zip(arg_types.iter().copied()) {
                if !match_type_pattern(pattern, arg_type, sema_ctx, &mut bindings) {
                    return Some(if decl.is_sfinae_guarded {
                        TemplateCallFailure::SfinaeRejected
                    } else {
                        TemplateCallFailure::DeductionFail
                    });
                }
            }
        }
        TemplateDeclKind::Class { constructors } => {
            if !constructors.is_empty()
                && !constructors.iter().any(|ctor| {
                    ctor.len() == arg_types.len() && ctor
                        .iter()
                        .zip(arg_types.iter().copied())
                        .all(|(pattern, arg_type)| {
                            let mut bindings = HashMap::new();
                            match_type_pattern(pattern, arg_type, sema_ctx, &mut bindings)
                        })
                })
            {
                return Some(if decl.is_sfinae_guarded {
                    TemplateCallFailure::SfinaeRejected
                } else {
                    TemplateCallFailure::DeductionFail
                });
            }
        }
    }

    if decl.sfinae_always_rejects {
        return Some(TemplateCallFailure::SfinaeRejected);
    }

    None
}

fn explicit_specialization_args_match(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
    sema_ctx: Option<&SemaContext>,
) -> Result<(), TemplateCallFailure> {
    if decl.specialization_args.is_empty() {
        return Ok(());
    }
    if explicit_args.is_empty() {
        return Ok(());
    }
    if explicit_args.len() != decl.specialization_args.len() {
        return Err(TemplateCallFailure::ExplicitArityMismatch);
    }

    for (expected, actual) in decl.specialization_args.iter().zip(explicit_args.iter()) {
        match (expected, actual) {
            (ExplicitTemplateArg::Type(expected), ExplicitTemplateArg::Type(actual)) => {
                if !template_arg_type_texts_match(expected, actual, sema_ctx) {
                    return Err(TemplateCallFailure::DeductionFail);
                }
            }
            (ExplicitTemplateArg::Value(expected), ExplicitTemplateArg::Value(actual)) => {
                if normalize_template_value(expected) != normalize_template_value(actual) {
                    return Err(TemplateCallFailure::NonTypeArgMismatch);
                }
            }
            (ExplicitTemplateArg::Type(_), ExplicitTemplateArg::Value(_))
            | (ExplicitTemplateArg::Value(_), ExplicitTemplateArg::Type(_)) => {
                return Err(TemplateCallFailure::NonTypeArgMismatch);
            }
        }
    }

    Ok(())
}

fn bind_explicit_template_arg(
    param: &TemplateParam,
    explicit_arg: &ExplicitTemplateArg,
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> std::result::Result<(), TemplateCallFailure> {
    match (param, explicit_arg) {
        (TemplateParam::TypeParam { name, .. }, ExplicitTemplateArg::Type(value)) => {
            let binding = resolve_type_name_existing(sema_ctx, value)
                .map(TemplateBinding::Type)
                .unwrap_or_else(|| TemplateBinding::Text(value.clone()));
            bindings.insert(name.clone(), binding);
            Ok(())
        }
        (TemplateParam::NonTypeParam { name, .. }, ExplicitTemplateArg::Value(value)) => {
            bindings.insert(name.clone(), TemplateBinding::Value(value.clone()));
            Ok(())
        }
        (TemplateParam::NonTypeParam { .. }, ExplicitTemplateArg::Type(_)) => {
            Err(TemplateCallFailure::NonTypeArgMismatch)
        }
        (TemplateParam::TypeParam { .. }, ExplicitTemplateArg::Value(_)) => {
            Err(TemplateCallFailure::DeductionFail)
        }
    }
}

fn bind_default_template_arg(
    param: &TemplateParam,
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> std::result::Result<(), TemplateCallFailure> {
    let (name, default_arg) = match param {
        TemplateParam::TypeParam {
            name,
            default_arg,
        }
        | TemplateParam::NonTypeParam {
            name,
            default_arg,
            ..
        } => (name, default_arg),
    };

    if bindings.contains_key(name) {
        return Ok(());
    }

    let Some(default_arg) = default_arg.as_ref() else {
        return Ok(());
    };

    bind_explicit_template_arg(param, default_arg, sema_ctx, bindings)
}

fn match_type_pattern(
    pattern: &TypePattern,
    arg_type: TypeId,
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> bool {
    match pattern {
        TypePattern::TemplateParam(name) => bind_template_type(name, arg_type, sema_ctx, bindings),
        TypePattern::NonTypeParam(_) | TypePattern::Value(_) => false,
        TypePattern::Reference(inner) => match_type_pattern(inner, arg_type, sema_ctx, bindings),
        TypePattern::Pointer(inner) => {
            if let Some(TypeKind::Pointer { pointee, .. }) = sema_ctx.types.get(arg_type) {
                match_type_pattern(inner, *pointee, sema_ctx, bindings)
            } else {
                false
            }
        }
        TypePattern::TemplateInstance { base, args } => {
            let Some(TypeKind::Template { base: actual_base, args: actual_args }) =
                sema_ctx.types.get(arg_type)
            else {
                return false;
            };
            if actual_base != base || actual_args.len() != args.len() {
                return false;
            }
            args.iter()
                .zip(actual_args.iter())
                .all(|(expected, actual)| match (expected, actual) {
                    (TypePattern::TemplateParam(name), TemplateArg::Type(type_id)) => {
                        bind_template_type(name, *type_id, sema_ctx, bindings)
                    }
                    (TypePattern::NonTypeParam(name), TemplateArg::Value(value)) => {
                        bind_template_value(name, value, bindings)
                    }
                    (TypePattern::Concrete(expected_text), TemplateArg::Type(type_id)) => {
                        resolve_type_name_existing(sema_ctx, expected_text)
                            .map(|expected_id| sema_ctx.types_equivalent(expected_id, *type_id))
                            .unwrap_or_else(|| render_type(sema_ctx, *type_id) == *expected_text)
                    }
                    (TypePattern::Value(expected), TemplateArg::Value(actual)) => expected == actual,
                    (
                        TypePattern::TemplateInstance { .. }
                        | TypePattern::Pointer(_)
                        | TypePattern::Reference(_),
                        TemplateArg::Type(type_id),
                    ) => match_type_pattern(expected, *type_id, sema_ctx, bindings),
                    _ => false,
                })
        }
        TypePattern::Concrete(expected) => resolve_type_name_existing(sema_ctx, expected)
            .map(|expected_id| sema_ctx.types_equivalent(expected_id, arg_type))
            .unwrap_or_else(|| render_type(sema_ctx, arg_type) == *expected),
    }
}

fn bind_template_type(
    name: &str,
    arg_type: TypeId,
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> bool {
    match bindings.get(name) {
        Some(TemplateBinding::Type(existing)) => sema_ctx.types_equivalent(*existing, arg_type),
        Some(TemplateBinding::Text(existing)) => resolve_type_name_existing(sema_ctx, existing)
            .map(|expected_id| sema_ctx.types_equivalent(expected_id, arg_type))
            .unwrap_or_else(|| render_type(sema_ctx, arg_type) == *existing),
        Some(TemplateBinding::Value(_)) => false,
        None => {
            bindings.insert(name.to_string(), TemplateBinding::Type(arg_type));
            true
        }
    }
}

fn bind_template_value(
    name: &str,
    value: &str,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> bool {
    match bindings.get(name) {
        Some(TemplateBinding::Value(existing)) => existing == value,
        Some(TemplateBinding::Text(_) | TemplateBinding::Type(_)) => false,
        None => {
            bindings.insert(name.to_string(), TemplateBinding::Value(value.to_string()));
            true
        }
    }
}

fn bind_class_template_ctor(
    decl: &TemplateDecl,
    constructors: &[Vec<TypePattern>],
    arg_types: &[TypeId],
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> bool {
    if constructors.is_empty() {
        return arg_types.is_empty() || bindings.len() == decl.params.len();
    }

    for ctor in constructors {
        if ctor.len() != arg_types.len() {
            continue;
        }
        let mut local_bindings = bindings.clone();
        let matched = ctor
            .iter()
            .zip(arg_types.iter())
            .all(|(pattern, arg_type)| match_type_pattern(pattern, *arg_type, sema_ctx, &mut local_bindings));
        if matched {
            *bindings = local_bindings;
            return true;
        }
    }

    false
}

fn matches_non_type_binding(binding: &TemplateBinding, declared_type: &str) -> bool {
    match binding {
        TemplateBinding::Value(value) => matches_non_type_value(value, declared_type),
        _ => false,
    }
}

fn matches_non_type_value(value: &str, declared_type: &str) -> bool {
    match declared_type {
        "bool" => matches!(value, "true" | "false" | "TRUE" | "FALSE"),
        "char" => value.starts_with('\'') && value.ends_with('\''),
        _ => value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '-' | '+')),
    }
}

fn call_argument_types(call: Node, sema_ctx: &SemaContext) -> Vec<TypeId> {
    let Some(args) =
        call.child_by_field_name("arguments").or_else(|| find_descendant(call, "argument_list"))
    else {
        return Vec::new();
    };
    let mut types = Vec::new();
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if let Some(type_id) = super::expr::type_of_expression(sema_ctx, child) {
            types.push(type_id);
        }
    }
    types
}

fn template_callee_name(callee: Node, sema_ctx: &SemaContext) -> Option<String> {
    let source = sema_ctx.source()?;
    let raw = match callee.kind() {
        "template_function" | "template_method" => callee
            .child_by_field_name("name")
            .and_then(|name| node_text(name, source))
            .map(|text| text.trim().to_string()),
        "identifier" | "field_identifier" => {
            node_text(callee, source).map(|text| text.trim().to_string())
        }
        "qualified_identifier" => node_text(callee, source).map(|text| text.trim().to_string()),
        _ => None,
    }?;
    let stripped = strip_template_suffix(raw.trim());
    (!stripped.is_empty()).then(|| stripped.to_string())
}

fn template_type_name<'a>(template_type: Node, sema_ctx: &'a SemaContext) -> Option<&'a str> {
    let source = sema_ctx.source()?;
    let name_node = template_type.child_by_field_name("name")?;
    let raw = node_text(name_node, source)?.trim();
    let stripped = strip_template_suffix(raw);
    (!stripped.is_empty()).then_some(stripped)
}

fn explicit_specialization_args(node: Node, sema_ctx: &SemaContext) -> Vec<ExplicitTemplateArg> {
    let Some(source) = sema_ctx.source() else {
        return Vec::new();
    };
    let Some(arg_list) = explicit_specialization_arg_list(node) else {
        return Vec::new();
    };
    explicit_template_args_from_arg_list(arg_list, source, sema_ctx)
}

fn explicit_specialization_arg_list(node: Node) -> Option<Node> {
    if let Some(class_node) = find_template_class_node(node) {
        if let Some(name_node) = class_node.child_by_field_name("name")
            && let Some(arg_list) = find_descendant(name_node, "template_argument_list")
        {
            return Some(arg_list);
        }
        if let Some(arg_list) = find_descendant(class_node, "template_argument_list") {
            return Some(arg_list);
        }
    }

    let function_node =
        find_descendant(node, "function_definition").or_else(|| find_descendant(node, "declaration"))?;
    let declarator = function_node
        .child_by_field_name("declarator")
        .or_else(|| find_descendant(function_node, "function_declarator"))?;
    find_descendant(declarator, "template_argument_list")
}

pub fn explicit_template_args(callee: Node, sema_ctx: &SemaContext) -> Vec<ExplicitTemplateArg> {
    let Some(source) = sema_ctx.source() else {
        return Vec::new();
    };
    let Some(arg_list) = find_descendant(callee, "template_argument_list") else {
        return Vec::new();
    };
    explicit_template_args_from_arg_list(arg_list, source, sema_ctx)
}

pub fn explicit_template_args_from_type(
    template_type: Node,
    sema_ctx: &SemaContext,
) -> Vec<ExplicitTemplateArg> {
    let Some(source) = sema_ctx.source() else {
        return Vec::new();
    };
    let Some(arg_list) = template_type.child_by_field_name("arguments") else {
        return Vec::new();
    };
    explicit_template_args_from_arg_list(arg_list, source, sema_ctx)
}

fn explicit_template_args_from_arg_list(
    arg_list: Node,
    source: &str,
    sema_ctx: &SemaContext,
) -> Vec<ExplicitTemplateArg> {
    let mut args = Vec::new();
    let mut cursor = arg_list.walk();
    for child in arg_list.children(&mut cursor) {
        let kind = child.kind();
        match kind {
            "type_descriptor" | "type_identifier" | "primitive_type" | "qualified_identifier" | "template_type" => {
                let text = crate::parser::cpp::clean_type_string(node_text(child, source).unwrap_or(""));
                if !text.is_empty() {
                    args.push(ExplicitTemplateArg::Type(text));
                }
            }
            "number_literal" | "char_literal" | "true" | "false" | "identifier" | "field_identifier" => {
                let text = node_text(child, source).unwrap_or("").trim().to_string();
                if !text.is_empty() {
                    if matches!(kind, "identifier" | "field_identifier")
                        && sema_ctx.resolve_existing_type_text(&text).is_some()
                    {
                        args.push(ExplicitTemplateArg::Type(text));
                    } else {
                        args.push(ExplicitTemplateArg::Value(text));
                    }
                }
            }
            _ => {}
        }
    }
    args
}

fn ordered_template_decls(decls: &[TemplateDecl]) -> Vec<&TemplateDecl> {
    let mut ordered = decls.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|decl| !decl.is_explicit_specialization);
    ordered
}

fn instantiate_specialization_decl_type(
    decl: &TemplateDecl,
    sema_ctx: &SemaContext,
) -> Option<TypeId> {
    if decl.specialization_args.is_empty() {
        return None;
    }
    let rendered_args = decl
        .specialization_args
        .iter()
        .map(|arg| match arg {
            ExplicitTemplateArg::Type(value) | ExplicitTemplateArg::Value(value) => value.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let rendered = format!("{}<{}>", decl.name, rendered_args);
    sema_ctx.resolve_existing_type_text(&rendered)
}

fn template_arg_type_texts_match(
    expected: &str,
    actual: &str,
    sema_ctx: Option<&SemaContext>,
) -> bool {
    let expected = crate::parser::cpp::clean_type_string(expected);
    let actual = crate::parser::cpp::clean_type_string(actual);
    if expected == actual {
        return true;
    }
    let Some(sema_ctx) = sema_ctx else {
        return false;
    };
    match (
        sema_ctx.resolve_existing_type_text(&expected),
        sema_ctx.resolve_existing_type_text(&actual),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn normalize_template_value(text: &str) -> String {
    text.split_whitespace().collect::<String>()
}

fn normalize_signature_key(function_node: Node, source: &str, name: &str) -> String {
    let return_type = function_node
        .child_by_field_name("type")
        .and_then(|node| node_text(node, source))
        .map(crate::parser::cpp::clean_type_string)
        .unwrap_or_else(|| "void".to_string());
    let declarator = function_node
        .child_by_field_name("declarator")
        .and_then(|node| node_text(node, source))
        .unwrap_or("")
        .replace(char::is_whitespace, "");
    format!("{return_type}:{name}:{declarator}")
}

fn normalize_class_signature_key(class_node: Node, source: &str, name: &str) -> String {
    let text = node_text(class_node, source)
        .unwrap_or("")
        .replace(char::is_whitespace, "");
    format!("class:{name}:{text}")
}

fn instantiate_return_pattern(
    pattern: &TypePattern,
    bindings: &HashMap<String, TemplateBinding>,
    sema_ctx: &SemaContext,
) -> Option<TypeId> {
    match pattern {
        TypePattern::TemplateParam(name) => match bindings.get(name)? {
            TemplateBinding::Type(type_id) => Some(*type_id),
            TemplateBinding::Text(text) => resolve_type_name_existing(sema_ctx, text),
            TemplateBinding::Value(_) => None,
        },
        TypePattern::NonTypeParam(_) | TypePattern::Value(_) => None,
        TypePattern::Pointer(inner) => {
            let pointee = instantiate_return_pattern(inner, bindings, sema_ctx)?;
            sema_ctx.types.find(&TypeKind::Pointer {
                pointee,
                cv: CvQual::default(),
            })
        }
        TypePattern::Reference(inner) => {
            let referent = instantiate_return_pattern(inner, bindings, sema_ctx)?;
            sema_ctx.types.find(&TypeKind::Reference {
                referent,
                kind: RefKind::LValue,
            })
        }
        TypePattern::TemplateInstance { base, args } => {
            if is_enable_if_base(base) {
                return instantiate_enable_if_return(args, bindings, sema_ctx);
            }
            let mut instantiated_args = Vec::new();
            for arg in args {
                instantiated_args.push(match arg {
                    TypePattern::NonTypeParam(name) => {
                        TemplateArg::Value(binding_value(bindings, name)?.to_string())
                    }
                    TypePattern::Value(value) => TemplateArg::Value(value.clone()),
                    _ => TemplateArg::Type(instantiate_return_pattern(arg, bindings, sema_ctx)?),
                });
            }
            sema_ctx.types.find(&TypeKind::Template {
                base: base.clone(),
                args: instantiated_args,
            })
        }
        TypePattern::Concrete(text) => resolve_type_name_existing(sema_ctx, text),
    }
}

fn instantiate_enable_if_return(
    args: &[TypePattern],
    bindings: &HashMap<String, TemplateBinding>,
    sema_ctx: &SemaContext,
) -> Option<TypeId> {
    let condition = args.first()?;
    if !template_condition_value(condition, bindings, sema_ctx)? {
        return None;
    }

    if let Some(result_pattern) = args.get(1) {
        instantiate_return_pattern(result_pattern, bindings, sema_ctx)
    } else {
        sema_ctx.resolve_existing_type_text("void")
    }
}

fn template_condition_value(
    pattern: &TypePattern,
    bindings: &HashMap<String, TemplateBinding>,
    sema_ctx: &SemaContext,
) -> Option<bool> {
    match pattern {
        TypePattern::Value(value) | TypePattern::Concrete(value) => {
            parse_bool_token(value).or_else(|| {
                bindings
                    .get(value)
                    .and_then(|binding| template_binding_bool(binding, sema_ctx))
            })
        }
        TypePattern::NonTypeParam(name) | TypePattern::TemplateParam(name) => bindings
            .get(name)
            .and_then(|binding| template_binding_bool(binding, sema_ctx)),
        _ => None,
    }
}

fn template_binding_bool(binding: &TemplateBinding, sema_ctx: &SemaContext) -> Option<bool> {
    match binding {
        TemplateBinding::Value(value) | TemplateBinding::Text(value) => parse_bool_token(value),
        TemplateBinding::Type(type_id) => sema_ctx
            .render_type(*type_id)
            .as_deref()
            .and_then(parse_bool_token),
    }
}

fn parse_bool_token(text: &str) -> Option<bool> {
    match normalize_template_value(text).as_str() {
        "true" | "TRUE" => Some(true),
        "false" | "FALSE" => Some(false),
        _ => None,
    }
}

fn instantiate_decl_type(
    decl: &TemplateDecl,
    bindings: &HashMap<String, TemplateBinding>,
    sema_ctx: &SemaContext,
) -> Option<TypeId> {
    let mut args = Vec::with_capacity(decl.params.len());
    for param in &decl.params {
        match param {
            TemplateParam::TypeParam { name, .. } => {
                let binding = bindings.get(name)?;
                let type_id = match binding {
                    TemplateBinding::Type(type_id) => *type_id,
                    TemplateBinding::Text(text) => resolve_type_name_existing(sema_ctx, text)?,
                    TemplateBinding::Value(_) => return None,
                };
                args.push(TemplateArg::Type(type_id));
            }
            TemplateParam::NonTypeParam { name, .. } => {
                args.push(TemplateArg::Value(binding_value(bindings, name)?.to_string()));
            }
        }
    }
    sema_ctx.types.find(&TypeKind::Template {
        base: decl.name.clone(),
        args,
    })
}

fn binding_value<'a>(
    bindings: &'a HashMap<String, TemplateBinding>,
    name: &str,
) -> Option<&'a str> {
    match bindings.get(name)? {
        TemplateBinding::Value(value) => Some(value.as_str()),
        _ => None,
    }
}

fn resolve_type_name_existing(sema_ctx: &SemaContext, text: &str) -> Option<TypeId> {
    sema_ctx.resolve_existing_type_text(text)
}

fn render_type(sema_ctx: &SemaContext, type_id: TypeId) -> String {
    sema_ctx
        .render_type(type_id)
        .unwrap_or_else(|| "unknown".to_string())
}

fn template_lookup_keys(name: &str) -> Vec<String> {
    let mut keys = vec![name.to_string()];
    let short = short_name(name);
    if short != name {
        keys.push(short.to_string());
    }
    keys
}

fn short_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn strip_template_suffix(name: &str) -> &str {
    name.split('<').next().unwrap_or(name).trim()
}

fn template_type_base_name(template_type: Node, source: &str) -> Option<String> {
    let name_node = template_type.child_by_field_name("name")?;
    let raw = crate::parser::cpp::clean_type_string(node_text(name_node, source)?);
    let stripped = strip_template_suffix(&raw).trim();
    (!stripped.is_empty()).then(|| stripped.to_string())
}

fn template_type_args(template_type: Node, source: &str) -> Vec<String> {
    let Some(arg_list) = template_type.child_by_field_name("arguments") else {
        return Vec::new();
    };

    let mut args = Vec::new();
    let mut cursor = arg_list.walk();
    for child in arg_list.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        let text = node_text(child, source).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        args.push(crate::parser::cpp::clean_type_string(text));
    }
    args
}

fn is_enable_if_base(base: impl AsRef<str>) -> bool {
    let base = base.as_ref();
    matches!(base.rsplit("::").next().unwrap_or(base), "enable_if_t" | "enable_if")
}

fn qualify_template_decl_name(node: Node, local_name: &str, source: &str) -> String {
    let mut qualifiers = Vec::<String>::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "namespace_definition" => {
                if let Some(name_node) = parent.child_by_field_name("name")
                    && let Some(text) = node_text(name_node, source)
                {
                    let text = text.trim();
                    if !text.is_empty() {
                        qualifiers.push(text.to_string());
                    }
                }
            }
            "class_specifier"
            | "struct_specifier"
            | "unreal_reflected_class_declaration"
            | "unreal_reflected_struct_declaration" => {
                if let Some(name_node) = class_like_name_node(parent)
                    && let Some(text) = node_text(name_node, source)
                {
                    let text = text.trim();
                    if !text.is_empty() {
                        qualifiers.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent.parent();
    }

    qualifiers.reverse();
    if qualifiers.is_empty() {
        local_name.to_string()
    } else {
        format!("{}::{local_name}", qualifiers.join("::"))
    }
}

fn find_template_class_node(node: Node) -> Option<Node> {
    find_descendant(node, "class_specifier").or_else(|| find_descendant(node, "struct_specifier"))
}

fn class_like_name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
        .or_else(|| find_descendant(node, "type_identifier"))
        .or_else(|| find_descendant(node, "identifier"))
}

fn parse_class_template_constructors(
    class_node: Node,
    source: &str,
    template_param_kinds: &HashMap<String, bool>,
    class_name: &str,
) -> Vec<Vec<TypePattern>> {
    let Some(body) = find_descendant(class_node, "field_declaration_list")
        .or_else(|| find_descendant(class_node, "unreal_class_body"))
        .or_else(|| find_descendant(class_node, "unreal_struct_body"))
    else {
        return Vec::new();
    };

    let mut constructors = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if !matches!(
            child.kind(),
            "declaration"
                | "field_declaration"
                | "function_definition"
                | "unreal_function_definition"
                | "unreal_reflected_function"
        ) {
            continue;
        }
        let Some(declarator) = child
            .child_by_field_name("declarator")
            .or_else(|| find_descendant(child, "function_declarator"))
        else {
            continue;
        };
        let Some(name_node) = find_name_node(declarator) else {
            continue;
        };
        let name = node_text(name_node, source).unwrap_or("").trim();
        if name != class_name {
            continue;
        }
        constructors.push(parse_function_param_patterns(
            child,
            source,
            template_param_kinds,
        ));
    }

    constructors
}

fn build_template_param_kind_map(params: &[TemplateParam]) -> HashMap<String, bool> {
    let mut out = HashMap::with_capacity(params.len());
    for param in params {
        match param {
            TemplateParam::TypeParam { name, .. } => {
                out.insert(name.clone(), true);
            }
            TemplateParam::NonTypeParam { name, .. } => {
                out.insert(name.clone(), false);
            }
        }
    }
    out
}

fn parse_type_param_default(node: Node, source: &str) -> Option<ExplicitTemplateArg> {
    let text = node_text(node, source)?;
    let (_, default_text) = text.split_once('=')?;
    let clean = crate::parser::cpp::clean_type_string(default_text);
    (!clean.is_empty()).then_some(ExplicitTemplateArg::Type(clean))
}

fn parse_non_type_param_default(node: Node, source: &str) -> Option<ExplicitTemplateArg> {
    let text = node_text(node, source)?;
    let (_, default_text) = text.split_once('=')?;
    let value = default_text.trim().to_string();
    (!value.is_empty()).then_some(ExplicitTemplateArg::Value(value))
}

fn apply_default_template_args(
    params: &[TemplateParam],
    explicit_args: &[ExplicitTemplateArg],
    allow_missing_for_deduction: bool,
) -> std::result::Result<Vec<ExplicitTemplateArg>, TemplateCallFailure> {
    if explicit_args.len() > params.len() {
        return Err(TemplateCallFailure::ExplicitArityMismatch);
    }

    let mut out = explicit_args.to_vec();
    for param in params.iter().skip(explicit_args.len()) {
        let default_arg = match param {
            TemplateParam::TypeParam { default_arg, .. }
            | TemplateParam::NonTypeParam { default_arg, .. } => default_arg.clone(),
        };
        if let Some(default_arg) = default_arg {
            out.push(default_arg);
        } else if !allow_missing_for_deduction {
            return Err(TemplateCallFailure::ExplicitArityMismatch);
        }
    }

    Ok(out)
}

fn has_template_argument_list(node: Node) -> bool {
    find_descendant(node, "template_argument_list").is_some()
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

fn node_text<'a>(node: Node, source: &'a str) -> Option<&'a str> {
    let range = node.byte_range();
    if range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
    {
        Some(&source[range.start..range.end])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{TemplateCallFailure, TemplateIndex};
    use crate::sema::builder::build_sema;
    use tree_sitter::Parser;

    fn parse_root(content: &str) -> tree_sitter::Node<'_> {
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(content, None).unwrap();
        Box::leak(Box::new(tree)).root_node()
    }

    fn first_call(root: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
        fn find(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
            if node.kind() == "call_expression" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find(child) {
                    return Some(found);
                }
            }
            None
        }

        find(root).unwrap()
    }

    fn first_template_type(root: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
        fn find(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
            if node.kind() == "template_type" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find(child) {
                    return Some(found);
                }
            }
            None
        }

        find(root).unwrap()
    }

    #[test]
    fn template_index_reports_deduction_fail() {
        let content = "template<typename T> void Pair(T A, T B) {} void Test(){ Pair(1, \"x\"); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_call(first_call(root), &sema).unwrap();
        assert!(matches!(analysis.failure, TemplateCallFailure::DeductionFail));
    }

    #[test]
    fn template_index_reports_explicit_arity_mismatch() {
        let content =
            "template<typename T> T Id(T Value) { return Value; } void Test(){ Id<int32, float>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_call(first_call(root), &sema).unwrap();
        assert!(matches!(
            analysis.failure,
            TemplateCallFailure::ExplicitArityMismatch
        ));
    }

    #[test]
    fn template_index_reports_non_type_arg_mismatch() {
        let content = "template<int N> void Sized() {} void Test(){ Sized<int32>(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_call(first_call(root), &sema).unwrap();
        assert!(matches!(
            analysis.failure,
            TemplateCallFailure::NonTypeArgMismatch
        ));
    }

    #[test]
    fn template_index_tracks_specialization_conflicts() {
        let content = r#"
template<typename T> void Spec(T Value) {}
template<> void Spec<int32>(int32 Value) {}
template<> void Spec<int32>(int32 Value) {}
"#;
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        assert_eq!(index.specialization_conflicts().len(), 1);
        assert_eq!(index.specialization_conflict_entries().len(), 2);
    }

    #[test]
    fn template_index_validates_class_template_arg_kind() {
        let content = "template<int N> struct Sized {}; Sized<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index
            .analyze_template_type(first_template_type(root), &sema)
            .unwrap();
        assert!(matches!(
            analysis.failure,
            TemplateCallFailure::NonTypeArgMismatch
        ));
    }

    #[test]
    fn template_index_deduces_class_template_ctor_type() {
        let content = "template<typename T> struct Box { Box(T Value) {} }; Box<int32> Seed(); void Test(){ Box(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "Box<int32>");
    }

    #[test]
    fn template_index_resolves_alias_in_explicit_template_args() {
        let content =
            "using FCount = int32; template<typename T> T Id(T Value) { return Value; } void Test(){ Id<FCount>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "FCount");
    }

    #[test]
    fn template_index_resolves_namespace_qualified_template_call() {
        let content = "namespace UE::Math { template<typename T> T Id(T Value) { return Value; } } void Test(){ UE::Math::Id<int32>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let call = first_call(root);
        let ty = index
            .infer_call_return_type(call, &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn template_index_deduces_nested_pointer_reference_template_param() {
        let content =
            "template<typename T> T* Id(T*& Value) { return Value; } int32 Test(int32* Ptr) { return *Id(Ptr); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32*");
    }

    #[test]
    fn template_index_accepts_alias_type_argument_in_template_type() {
        let content = "using FCount = int32; template<typename T> struct Box {}; Box<FCount> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_accepts_namespace_qualified_template_type() {
        let content =
            "namespace UE::Math { template<typename T> struct Box {}; } UE::Math::Box<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_reports_sfinae_rejected_class_template_type() {
        let content =
            "template<typename T> requires false struct Box {}; Box<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index
            .analyze_template_type(first_template_type(root), &sema)
            .unwrap();
        assert!(matches!(
            analysis.failure,
            TemplateCallFailure::SfinaeRejected
        ));
    }

    #[test]
    fn template_index_accepts_explicit_class_specialization_type() {
        let content =
            "template<typename T> struct Box {}; template<> struct Box<int32> {}; Box<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_accepts_alias_to_explicit_class_specialization_type() {
        let content =
            "using FCount = int32; template<typename T> struct Box {}; template<> struct Box<int32> {}; Box<FCount> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_accepts_namespace_qualified_explicit_class_specialization_type() {
        let content =
            "namespace UE::Math { template<typename T> struct Box {}; template<> struct Box<int32> {}; } UE::Math::Box<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_infers_enable_if_true_return_type() {
        let content =
            "template<typename T> std::enable_if_t<true, T> Only(T Value) { return Value; } void Test(){ Only(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn template_index_reports_requires_clause_with_extra_spacing_as_sfinae_rejected() {
        let content =
            "template<typename T> requires   false struct Box {}; Box<int32> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index
            .analyze_template_type(first_template_type(root), &sema)
            .unwrap();
        assert!(matches!(
            analysis.failure,
            TemplateCallFailure::SfinaeRejected
        ));
    }

    #[test]
    fn template_index_accepts_default_type_param_in_template_type() {
        let content = "template<typename T = int32> struct Box {}; Box<> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_accepts_default_type_param_in_template_call() {
        let content =
            "template<typename T = int32> T Id(T Value) { return Value; } void Test(){ Id<>(1); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn template_index_uses_default_type_param_without_explicit_brackets() {
        let content =
            "template<typename T = int32> T Make() { return 1; } int32 Test(){ return Make(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }

    #[test]
    fn template_index_prefers_deduced_type_over_default_param() {
        let content =
            "template<typename T = int32> T Id(T Value) { return Value; } float Test(){ return Id(1.0f); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "float");
    }

    #[test]
    fn template_index_accepts_default_non_type_param_in_template_type() {
        let content = "template<int N = 4> struct Sized {}; Sized<> Value;";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let analysis = index.analyze_template_type(first_template_type(root), &sema);
        assert!(analysis.is_none());
    }

    #[test]
    fn template_index_uses_default_non_type_param_without_explicit_brackets() {
        let content =
            "template<int N = 4> int32 Make() { return N; } int32 Test(){ return Make(); }";
        let root = parse_root(content);
        let sema = build_sema(root, content);
        let index = TemplateIndex::collect(root, &sema);
        let ty = index
            .infer_call_return_type(first_call(root), &sema)
            .and_then(|id| sema.render_type(id))
            .unwrap();
        assert_eq!(ty, "int32");
    }
}
