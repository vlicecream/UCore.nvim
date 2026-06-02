use anyhow::Result;

use crate::diagnostics::{DataflowRules, DiagnosticItem, DiagnosticSeverity, SemaContext};

pub(crate) fn collect(
    content: &str,
    file_path: Option<&str>,
    parsed_root: Option<tree_sitter::Node>,
    sema_ctx: Option<&SemaContext>,
    rules: &DataflowRules,
) -> Result<Vec<DiagnosticItem>> {
    let Some(root) = parsed_root else {
        return Ok(Vec::new());
    };
    let Some(sema_ctx) = sema_ctx else {
        return Ok(Vec::new());
    };

    let results = crate::sema::dataflow::analyze(root, content, sema_ctx);
    let mut items = Vec::new();
    for result in results {
        for issue in result.issues {
            let severity = match issue.code {
                "UECPP-DF-001" => DiagnosticSeverity::from(rules.unused_locals_severity),
                "UECPP-DF-002" => DiagnosticSeverity::from(rules.uninit_locals_severity),
                "UECPP-DF-003" => DiagnosticSeverity::from(rules.shadow_severity),
                _ => DiagnosticSeverity::Warning,
            };
            items.push(
                DiagnosticItem::new(
                    file_path,
                    issue.line,
                    issue.character,
                    severity,
                    "UCore",
                    issue.code,
                    issue.message,
                )
                .with_end(issue.end_line, issue.end_character),
            );
        }
    }

    Ok(items)
}
