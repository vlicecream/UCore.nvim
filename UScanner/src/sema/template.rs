use std::collections::HashMap;

use tree_sitter::Node;

use super::symbol::SourceRef;
use super::types::{CvQual, RefKind, TemplateArg, TypeId, TypeKind};
use super::SemaContext;

#[derive(Clone, Debug)]
pub enum TemplateParam {
    TypeParam { name: String },
    NonTypeParam { name: String, declared_type: String },
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
    pub signature_key: String,
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
        let has_explicit_args = !explicit_args.is_empty();

        let mut saw_arity = false;
        let mut saw_non_type_mismatch = false;
        let mut saw_sfinae = false;
        let mut saw_deduction_fail = false;

        for decl in decls {
            if has_explicit_args && explicit_args.len() > decl.params.len() {
                saw_arity = true;
                continue;
            }

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

        for decl in decls {
            let Ok(bindings) = bind_decl_call(decl, &explicit_args, &arg_types, sema_ctx) else {
                continue;
            };
            match &decl.kind {
                TemplateDeclKind::Function { return_pattern, .. } => {
                    if let Some(type_id) =
                        instantiate_return_pattern(return_pattern, &bindings, sema_ctx)
                    {
                        return Some(type_id);
                    }
                }
                TemplateDeclKind::Class { .. } => {
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
        let name_node = template_type.child_by_field_name("name")?;
        let name = node_text(name_node, sema_ctx.source()?)?.trim();
        if name.is_empty() {
            return None;
        }
        let decls = self.decls_by_name.get(name)?;
        let explicit_args = explicit_template_args_from_type(template_type, sema_ctx);
        if explicit_args.is_empty() {
            return None;
        }

        let mut saw_arity = false;
        let mut saw_non_type_mismatch = false;

        for decl in decls {
            if !matches!(decl.kind, TemplateDeclKind::Class { .. }) {
                continue;
            }
            match analyze_class_template_usage(decl, &explicit_args) {
                None => return None,
                Some(TemplateCallFailure::ExplicitArityMismatch) => saw_arity = true,
                Some(TemplateCallFailure::NonTypeArgMismatch) => saw_non_type_mismatch = true,
                Some(_) => {}
            }
        }

        let failure = if saw_arity {
            TemplateCallFailure::ExplicitArityMismatch
        } else if saw_non_type_mismatch {
            TemplateCallFailure::NonTypeArgMismatch
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
            index
                .decls_by_name
                .entry(decl.name.clone())
                .or_default()
                .push(decl);
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
        let name = node_text(name_node, source)?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let constructors =
            parse_class_template_constructors(class_node, source, &param_kinds, &name);
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
        let name = node_text(name_node, source)?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let function_params =
            parse_function_param_patterns(function_node, source, &param_kinds);
        let return_pattern = function_node
            .child_by_field_name("type")
            .map(|node| type_pattern_from_node(node, source, &param_kinds))
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
        is_sfinae_guarded: template_text.contains("enable_if")
            || template_text.contains("requires"),
        sfinae_always_rejects: template_text.contains("enable_if_t<false")
            || template_text.contains("enable_if<false")
            || template_text.contains("requires false"),
        is_explicit_specialization: template_text.trim_start().starts_with("template<>")
            || template_text.trim_start().starts_with("template <>"),
        signature_key,
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
                        params.push(TemplateParam::TypeParam { name });
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
                    params.push(TemplateParam::NonTypeParam { name, declared_type });
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
    let mut pattern = type_pattern_from_node(type_node, source, template_param_kinds);

    let mut cursor = node.child_by_field_name("declarator");
    while let Some(declarator) = cursor {
        pattern = match declarator.kind() {
            "pointer_declarator" => TypePattern::Pointer(Box::new(pattern)),
            "reference_declarator" => TypePattern::Reference(Box::new(pattern)),
            _ => pattern,
        };
        cursor = declarator.child_by_field_name("declarator");
    }

    Some(pattern)
}

fn type_pattern_from_node(
    node: Node,
    source: &str,
    template_param_kinds: &HashMap<String, bool>,
) -> TypePattern {
    match node.kind() {
        "template_type" => {
            let base = node
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source))
                .unwrap_or("")
                .trim()
                .to_string();
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

fn analyze_decl_call(
    decl: &TemplateDecl,
    explicit_args: &[ExplicitTemplateArg],
    arg_types: &[TypeId],
    sema_ctx: &SemaContext,
) -> Option<TemplateCallFailure> {
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
        match (param, explicit_arg) {
            (TemplateParam::TypeParam { name }, ExplicitTemplateArg::Type(value)) => {
                bindings.insert(name.clone(), TemplateBinding::Text(value.clone()));
            }
            (TemplateParam::NonTypeParam { name, .. }, ExplicitTemplateArg::Value(value)) => {
                bindings.insert(name.clone(), TemplateBinding::Value(value.clone()));
            }
            (TemplateParam::NonTypeParam { .. }, ExplicitTemplateArg::Type(_)) => {
                return Err(TemplateCallFailure::NonTypeArgMismatch);
            }
            (TemplateParam::TypeParam { .. }, ExplicitTemplateArg::Value(_)) => {
                return Err(TemplateCallFailure::DeductionFail);
            }
        }
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

    for param in &decl.params {
        match param {
            TemplateParam::TypeParam { name } => {
                if !bindings.contains_key(name) {
                    return Err(if decl.is_sfinae_guarded {
                        TemplateCallFailure::SfinaeRejected
                    } else {
                        TemplateCallFailure::DeductionFail
                    });
                }
            }
            TemplateParam::NonTypeParam { name, declared_type } => {
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
    if explicit_args.len() != decl.params.len() {
        return Some(TemplateCallFailure::ExplicitArityMismatch);
    }
    for (param, arg) in decl.params.iter().zip(explicit_args.iter()) {
        match (param, arg) {
            (TemplateParam::TypeParam { .. }, ExplicitTemplateArg::Type(_)) => {}
            (TemplateParam::NonTypeParam { declared_type, .. }, ExplicitTemplateArg::Value(value)) => {
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
    None
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
                        render_type(sema_ctx, *type_id) == *expected_text
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
        TypePattern::Concrete(expected) => render_type(sema_ctx, arg_type) == *expected,
    }
}

fn bind_template_type(
    name: &str,
    arg_type: TypeId,
    sema_ctx: &SemaContext,
    bindings: &mut HashMap<String, TemplateBinding>,
) -> bool {
    match bindings.get(name) {
        Some(TemplateBinding::Type(existing)) => *existing == arg_type,
        Some(TemplateBinding::Text(existing)) => render_type(sema_ctx, arg_type) == *existing,
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
    match callee.kind() {
        "template_function" | "template_method" => callee
            .child_by_field_name("name")
            .and_then(|name| node_text(name, source))
            .map(|text| text.trim().to_string()),
        "identifier" | "field_identifier" => {
            node_text(callee, source).map(|text| text.trim().to_string())
        }
        "qualified_identifier" => callee
            .child_by_field_name("name")
            .and_then(|name| node_text(name, source))
            .map(|text| text.trim().to_string()),
        _ => None,
    }
    .filter(|name| !name.is_empty())
}

pub fn explicit_template_args(callee: Node, sema_ctx: &SemaContext) -> Vec<ExplicitTemplateArg> {
    let Some(source) = sema_ctx.source() else {
        return Vec::new();
    };
    let Some(arg_list) = find_descendant(callee, "template_argument_list") else {
        return Vec::new();
    };
    explicit_template_args_from_arg_list(arg_list, source)
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
    explicit_template_args_from_arg_list(arg_list, source)
}

fn explicit_template_args_from_arg_list(arg_list: Node, source: &str) -> Vec<ExplicitTemplateArg> {
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
                    args.push(ExplicitTemplateArg::Value(text));
                }
            }
            _ => {}
        }
    }
    args
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

fn instantiate_decl_type(
    decl: &TemplateDecl,
    bindings: &HashMap<String, TemplateBinding>,
    sema_ctx: &SemaContext,
) -> Option<TypeId> {
    let mut args = Vec::with_capacity(decl.params.len());
    for param in &decl.params {
        match param {
            TemplateParam::TypeParam { name } => {
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
    match text.trim() {
        "void" => Some(sema_ctx.types.void_t),
        "bool" => Some(sema_ctx.types.bool_t),
        "char" | "TCHAR" | "ANSICHAR" => Some(sema_ctx.types_char()),
        "int" | "int32" => Some(sema_ctx.types.int32_t),
        "uint32" => Some(sema_ctx.types.uint32_t),
        "float" => Some(sema_ctx.types.float_t),
        "double" => Some(sema_ctx.types.double_t),
        other => sema_ctx
            .symbols
            .class_id_by_name(other)
            .and_then(|class_id| sema_ctx.types.find(&TypeKind::Class(class_id))),
    }
}

fn render_type(sema_ctx: &SemaContext, type_id: TypeId) -> String {
    sema_ctx
        .render_type(type_id)
        .unwrap_or_else(|| "unknown".to_string())
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
            TemplateParam::TypeParam { name } => {
                out.insert(name.clone(), true);
            }
            TemplateParam::NonTypeParam { name, .. } => {
                out.insert(name.clone(), false);
            }
        }
    }
    out
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
        | "bitfield_clause" => node.child_by_field_name("declarator").and_then(find_name_node),
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
}
