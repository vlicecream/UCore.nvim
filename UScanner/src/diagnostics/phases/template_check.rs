use anyhow::Result;
use tree_sitter::Node;

use crate::diagnostics::{DiagnosticItem, DiagnosticSeverity, TemplateRules};
use crate::sema::template::{TemplateCallFailure, TemplateIndex};
use crate::sema::SemaContext;

pub(crate) fn collect(
    file_path: Option<&str>,
    parsed_root: Option<Node>,
    sema_ctx: Option<&SemaContext>,
    rules: &TemplateRules,
) -> Result<Vec<DiagnosticItem>> {
    let Some(root) = parsed_root else {
        return Ok(Vec::new());
    };
    let Some(sema_ctx) = sema_ctx else {
        return Ok(Vec::new());
    };

    let template_index = TemplateIndex::collect(root, sema_ctx);
    let mut items = Vec::new();
    collect_call_items(root, file_path, sema_ctx, rules, &template_index, &mut items);
    collect_template_type_items(root, file_path, sema_ctx, rules, &template_index, &mut items);
    collect_specialization_items(file_path, rules, &template_index, &mut items);
    Ok(items)
}

fn collect_call_items(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TemplateRules,
    template_index: &TemplateIndex,
    items: &mut Vec<DiagnosticItem>,
) {
    if node.kind() == "call_expression" {
        if let Some(item) = call_template_item(node, file_path, sema_ctx, rules, template_index) {
            items.push(item);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_items(child, file_path, sema_ctx, rules, template_index, items);
    }
}

fn collect_template_type_items(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TemplateRules,
    template_index: &TemplateIndex,
    items: &mut Vec<DiagnosticItem>,
) {
    if node.kind() == "template_type" {
        if let Some(item) =
            template_type_item(node, file_path, sema_ctx, rules, template_index)
        {
            items.push(item);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_template_type_items(child, file_path, sema_ctx, rules, template_index, items);
    }
}

fn call_template_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TemplateRules,
    template_index: &TemplateIndex,
) -> Option<DiagnosticItem> {
    let analysis = template_index.analyze_call(node, sema_ctx)?;
    let callee = node.child_by_field_name("function").or_else(|| node.child(0))?;
    let (code, message, severity) = match analysis.failure {
        TemplateCallFailure::DeductionFail => (
            "UECPP-TPL-001",
            "Template argument deduction failed.",
            DiagnosticSeverity::from(rules.severity_deduction_fail),
        ),
        TemplateCallFailure::ExplicitArityMismatch => (
            "UECPP-TPL-002",
            "Explicit template argument count does not match template parameters.",
            DiagnosticSeverity::from(rules.severity_explicit_arity_mismatch),
        ),
        TemplateCallFailure::NonTypeArgMismatch => (
            "UECPP-TPL-003",
            "Non-type template argument does not match the template parameter kind.",
            DiagnosticSeverity::from(rules.severity_non_type_mismatch),
        ),
        TemplateCallFailure::SfinaeRejected => (
            "UECPP-TPL-004",
            "Template constraints or enable_if rejected all candidates.",
            DiagnosticSeverity::from(rules.severity_sfinae_rejected),
        ),
    };

    Some(
        DiagnosticItem::new(
            file_path,
            callee.start_position().row as u32,
            callee.start_position().column as u32,
            severity,
            "UCore",
            code,
            message,
        )
        .with_end(
            callee.end_position().row as u32,
            callee.end_position().column as u32,
        ),
    )
}

fn template_type_item(
    node: Node,
    file_path: Option<&str>,
    sema_ctx: &SemaContext,
    rules: &TemplateRules,
    template_index: &TemplateIndex,
) -> Option<DiagnosticItem> {
    let analysis = template_index.analyze_template_type(node, sema_ctx)?;
    let (code, message, severity) = match analysis.failure {
        TemplateCallFailure::ExplicitArityMismatch => (
            "UECPP-TPL-002",
            "Explicit template argument count does not match template parameters.",
            DiagnosticSeverity::from(rules.severity_explicit_arity_mismatch),
        ),
        TemplateCallFailure::NonTypeArgMismatch => (
            "UECPP-TPL-003",
            "Non-type template argument does not match the template parameter kind.",
            DiagnosticSeverity::from(rules.severity_non_type_mismatch),
        ),
        TemplateCallFailure::DeductionFail => (
            "UECPP-TPL-001",
            "Template argument deduction failed.",
            DiagnosticSeverity::from(rules.severity_deduction_fail),
        ),
        TemplateCallFailure::SfinaeRejected => (
            "UECPP-TPL-004",
            "Template constraints or enable_if rejected all candidates.",
            DiagnosticSeverity::from(rules.severity_sfinae_rejected),
        ),
    };

    Some(
        DiagnosticItem::new(
            file_path,
            node.start_position().row as u32,
            node.start_position().column as u32,
            severity,
            "UCore",
            code,
            message,
        )
        .with_end(node.end_position().row as u32, node.end_position().column as u32),
    )
}

fn collect_specialization_items(
    file_path: Option<&str>,
    rules: &TemplateRules,
    template_index: &TemplateIndex,
    items: &mut Vec<DiagnosticItem>,
) {
    for (key, range) in template_index.specialization_conflict_entries() {
        items.push(
            DiagnosticItem::new(
                file_path,
                range.start.line,
                range.start.column,
                DiagnosticSeverity::from(rules.severity_specialization_conflict),
                "UCore",
                "UECPP-TPL-005",
                format!("Template specialization conflicts with an existing specialization: {key}."),
            )
            .with_end(range.end.line, range.end.column.max(range.start.column + 1)),
        );
    }
}
