pub mod condition_eval;
pub mod expand;
pub mod macro_table;

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

pub use condition_eval::evaluate_condition;
pub use expand::preprocess_source;
pub use macro_table::MacroTable;

static PREDEFINED_MACROS: OnceLock<PredefinedMacrosFile> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct PreprocessResult {
    pub expanded_source: String,
    pub inactive_lines: HashSet<u32>,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct PredefinedMacrosFile {
    #[serde(default)]
    defines: PredefinedDefines,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct PredefinedDefines {
    #[serde(default)]
    predefined: Vec<String>,
}

pub fn default_macro_table() -> MacroTable {
    let parsed = PREDEFINED_MACROS.get_or_init(|| {
        toml::from_str(include_str!("../../data/predefined_macros.toml")).unwrap_or_default()
    });

    let mut table = MacroTable::default();
    for define in &parsed.defines.predefined {
        table.define_from_assignment(define);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::{default_macro_table, preprocess_source};

    #[test]
    fn preproc_suppresses_inactive_ifdef_branch() {
        let source = "#ifdef UE_BUILD_DEVELOPMENT\nint32 Value = 1;\n#else\n#error nope\n#endif\n";
        let result = preprocess_source(source, &default_macro_table());
        assert!(!result.inactive_lines.contains(&1));
        assert!(result.inactive_lines.contains(&3));
        assert!(result.expanded_source.contains("int32 Value = 1;"));
    }

    #[test]
    fn preproc_expands_simple_object_like_macros() {
        let source = "#define VALUE 7\nint32 Answer = VALUE;\n";
        let result = preprocess_source(source, &default_macro_table());
        assert!(result.expanded_source.contains("int32 Answer = 7;"));
    }
}
