use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Result;
use serde::Deserialize;

use crate::diagnostics::{
    DiagnosticItem, DiagnosticSeverity, IncludeResolutionRules,
};

static INCLUDE_ROOTS: OnceLock<IncludeRootsFile> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Default)]
struct IncludeRootsFile {
    #[serde(default)]
    ignore: IgnorePrefixes,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct IgnorePrefixes {
    #[serde(default)]
    prefixes: Vec<String>,
}

struct ParsedInclude<'a> {
    target: &'a str,
}

pub(crate) fn collect(
    content: &str,
    file_path: Option<&str>,
    rules: &IncludeResolutionRules,
) -> Result<Vec<DiagnosticItem>> {
    let Some(file_path) = file_path else {
        return Ok(Vec::new());
    };

    let roots = INCLUDE_ROOTS.get_or_init(|| load_include_roots(&rules.include_roots_file).unwrap_or_default());
    let resolver = crate::preproc::default_include_resolver_for_file(&rules.include_roots_file, Some(file_path));
    let severity = DiagnosticSeverity::from(rules.severity);
    let mut items = Vec::new();

    for (row, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(parsed) = parse_include_line(trimmed) else {
            continue;
        };

        if roots
            .ignore
            .prefixes
            .iter()
            .any(|prefix| parsed.target.starts_with(prefix))
        {
            continue;
        }

        if resolver.has_include(parsed.target) {
            continue;
        }

        let col = line.find('"').or_else(|| line.find('<')).unwrap_or(0) as u32;
        items.push(
            DiagnosticItem::new(
                Some(file_path),
                row as u32,
                col,
                severity.clone(),
                "UCore",
                "UECPP-INC-001",
                format!("'{}' file not found in any search path.", parsed.target),
            )
            .with_end(row as u32, line.len() as u32),
        );
    }

    Ok(items)
}

fn parse_include_line(line: &str) -> Option<ParsedInclude<'_>> {
    let after_hash = line.strip_prefix('#')?.trim_start();
    let after_include = after_hash.strip_prefix("include")?.trim_start();

    if let Some(rest) = after_include.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(ParsedInclude { target: &rest[..end] })
    } else if let Some(rest) = after_include.strip_prefix('<') {
        let end = rest.find('>')?;
        Some(ParsedInclude { target: &rest[..end] })
    } else {
        None
    }
}

fn load_include_roots(file_name: &str) -> Result<IncludeRootsFile> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = Path::new(manifest_dir).join("data").join(file_name);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|_| include_str!("../../../data/include_roots.toml").to_string());
    Ok(toml::from_str(&text).unwrap_or_default())
}
