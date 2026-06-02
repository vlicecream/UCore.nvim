pub mod condition_eval;
pub mod expand;
pub mod macro_table;
pub mod tokenizer;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

pub use condition_eval::evaluate_condition;
pub use expand::{preprocess_source, preprocess_source_with_resolver};
pub use macro_table::MacroTable;

static PREPROCESSOR_CONFIGS: OnceLock<Mutex<HashMap<String, PreprocessorConfigFile>>> = OnceLock::new();

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PreprocessResult {
    pub expanded_source: String,
    pub inactive_lines: HashSet<u32>,
    pub line_column_maps: Vec<Vec<u32>>,
}

impl PreprocessResult {
    pub fn map_column(&self, line: u32, expanded_column: u32) -> u32 {
        let Some(columns) = self.line_column_maps.get(line as usize) else {
            return expanded_column;
        };
        let index = expanded_column.min(columns.len().saturating_sub(1) as u32) as usize;
        columns.get(index).copied().unwrap_or(expanded_column)
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct PreprocessorConfigFile {
    #[serde(default)]
    pub defines: PredefinedDefines,
    #[serde(default)]
    pub include_paths: IncludePathConfig,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct PredefinedDefines {
    #[serde(default)]
    pub predefined: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct IncludePathConfig {
    #[serde(default)]
    pub project_search_dirs: Vec<String>,
    #[serde(default)]
    pub engine_search_dirs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct IncludeResolver {
    current_file: Option<PathBuf>,
    project_root: Option<PathBuf>,
    engine_root: Option<PathBuf>,
    project_search_dirs: Vec<String>,
    engine_search_dirs: Vec<String>,
}

impl IncludeResolver {
    pub fn has_include(&self, include: &str) -> bool {
        let normalized = normalize_include_operand(include);
        if normalized.is_empty() {
            return false;
        }

        let include_path = PathBuf::from(&normalized);
        if include_path.is_absolute() {
            return include_path.is_file();
        }

        if let Some(current_dir) = self.current_file.as_deref().and_then(Path::parent) {
            if current_dir.join(&include_path).is_file() {
                return true;
            }
        }

        self.search_roots()
            .into_iter()
            .any(|root| root.exists() && has_include_under_root(&root, &normalized))
    }

    fn search_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();

        if let Some(project_root) = &self.project_root {
            roots.extend(expand_search_dirs(
                project_root,
                &self.project_search_dirs,
                project_root.file_name().and_then(|name| name.to_str()),
            ));
        }

        if let Some(engine_root) = &self.engine_root {
            roots.extend(expand_search_dirs(
                engine_root,
                &self.engine_search_dirs,
                self.project_root
                    .as_deref()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str()),
            ));
        }

        roots
    }
}

pub fn default_macro_table() -> MacroTable {
    default_macro_table_for_file("preprocessor.toml")
}

pub fn preprocess_source_cached_with_resolver(
    source: &str,
    base_macros: &MacroTable,
    include_resolver: Option<&IncludeResolver>,
    current_file: Option<&str>,
) -> PreprocessResult {
    let Some(cache_path) = preprocess_cache_path(source, base_macros, current_file) else {
        return expand::preprocess_source_with_resolver(source, base_macros, include_resolver);
    };

    if let Ok(text) = fs::read_to_string(&cache_path) {
        if let Ok(cached) = serde_json::from_str::<PreprocessResult>(&text) {
            return cached;
        }
    }

    let result = expand::preprocess_source_with_resolver(source, base_macros, include_resolver);
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
        if let Ok(text) = serde_json::to_string(&result) {
            let _ = fs::write(cache_path, text);
        }
    }
    result
}

pub fn default_macro_table_for_file(file_name: &str) -> MacroTable {
    let parsed = default_preprocessor_config_for_file(file_name);

    let mut table = MacroTable::default();
    for define in &parsed.defines.predefined {
        table.define_from_assignment(define);
    }
    table
}

pub fn default_preprocessor_config_for_file(file_name: &str) -> PreprocessorConfigFile {
    let cache = PREPROCESSOR_CONFIGS.get_or_init(|| Mutex::new(HashMap::new()));
    let parsed = {
        let mut guard = cache.lock().unwrap();
        guard
            .entry(file_name.to_string())
            .or_insert_with(|| load_preprocessor_config(file_name))
            .clone()
    };
    parsed
}

pub fn default_include_resolver_for_file(file_name: &str, current_file: Option<&str>) -> IncludeResolver {
    let config = default_preprocessor_config_for_file(file_name);
    let current_file = current_file.map(PathBuf::from);
    let project_root = current_file
        .as_deref()
        .and_then(find_project_root_for_file)
        .or_else(|| current_file.as_deref().and_then(find_engine_embedded_project_root));
    let engine_root = current_file
        .as_deref()
        .and_then(find_engine_root_for_file)
        .or_else(|| project_root.as_deref().and_then(find_engine_root_near_project));

    IncludeResolver {
        current_file,
        project_root,
        engine_root,
        project_search_dirs: config.include_paths.project_search_dirs,
        engine_search_dirs: config.include_paths.engine_search_dirs,
    }
}

fn load_preprocessor_config(file_name: &str) -> PreprocessorConfigFile {
    let default_text = include_str!("../../data/preprocessor.toml");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = Path::new(manifest_dir).join("data").join(file_name);
    let text = fs::read_to_string(path).unwrap_or_else(|_| default_text.to_string());
    let mut parsed = toml::from_str(&text).unwrap_or_default();
    merge_include_roots(&mut parsed);
    parsed
}

fn preprocess_cache_path(
    source: &str,
    base_macros: &MacroTable,
    current_file: Option<&str>,
) -> Option<PathBuf> {
    let current_file = current_file?;
    let project_root = find_project_root_for_file(Path::new(current_file))
        .or_else(|| find_engine_embedded_project_root(Path::new(current_file)))?;
    let mtime = fs::metadata(current_file)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let defines_hash = base_macros.defines_hash();
    let mut hasher = blake3::Hasher::new();
    hasher.update(current_file.replace('\\', "/").as_bytes());
    hasher.update(&mtime.to_le_bytes());
    hasher.update(source.as_bytes());
    hasher.update(defines_hash.as_bytes());
    let file_name = format!("{}.json", hasher.finalize().to_hex());
    Some(project_root.join(".ucore").join("preproc").join(file_name))
}

fn merge_include_roots(config: &mut PreprocessorConfigFile) {
    let roots: PreprocessorConfigFile =
        toml::from_str(include_str!("../../data/include_roots.toml")).unwrap_or_default();

    for dir in roots.include_paths.project_search_dirs {
        if !config.include_paths.project_search_dirs.iter().any(|existing| existing == &dir) {
            config.include_paths.project_search_dirs.push(dir);
        }
    }

    for dir in roots.include_paths.engine_search_dirs {
        if !config.include_paths.engine_search_dirs.iter().any(|existing| existing == &dir) {
            config.include_paths.engine_search_dirs.push(dir);
        }
    }
}

fn normalize_include_operand(include: &str) -> String {
    include
        .trim()
        .trim_start_matches('<')
        .trim_start_matches('"')
        .trim_end_matches('>')
        .trim_end_matches('"')
        .replace('\\', "/")
}

fn expand_search_dirs(root: &Path, dirs: &[String], project_name: Option<&str>) -> Vec<PathBuf> {
    dirs.iter()
        .map(|dir| {
            let expanded = project_name
                .map(|name| dir.replace("${ProjectName}", name))
                .unwrap_or_else(|| dir.clone());
            root.join(expanded)
        })
        .collect()
}

fn has_include_under_root(root: &Path, include: &str) -> bool {
    if root.join(include).is_file() {
        return true;
    }

    let include_suffix = include.replace('\\', "/");
    let target_name = Path::new(&include_suffix)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if target_name.is_empty() {
        return false;
    }

    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .build()
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_file()).unwrap_or(false))
        .any(|entry| {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if file_name != target_name {
                return false;
            }

            let relative = path
                .strip_prefix(root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            relative.ends_with(&include_suffix)
        })
}

fn find_project_root_for_file(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if contains_uproject(ancestor) {
            return Some(ancestor.to_path_buf());
        }
        if ancestor.join("Source").is_dir() && path.starts_with(ancestor.join("Source")) {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn find_engine_embedded_project_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join("Source").is_dir() && contains_uproject(ancestor))
        .map(Path::to_path_buf)
}

fn find_engine_root_for_file(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| is_engine_root(ancestor))
        .map(Path::to_path_buf)
}

fn find_engine_root_near_project(project_root: &Path) -> Option<PathBuf> {
    project_root
        .ancestors()
        .find(|ancestor| is_engine_root(ancestor))
        .map(Path::to_path_buf)
}

fn contains_uproject(path: &Path) -> bool {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("uproject"))
                .unwrap_or(false)
        })
}

fn is_engine_root(path: &Path) -> bool {
    path.join("Engine/Source").is_dir()
        || path.join("Engine/Plugins").is_dir()
        || path.join("Engine/Build/Build.version").is_file()
}

#[cfg(test)]
mod tests {
    use super::{
        default_include_resolver_for_file, default_macro_table, default_macro_table_for_file,
        default_preprocessor_config_for_file, preprocess_source,
        preprocess_source_cached_with_resolver, preprocess_source_with_resolver,
    };
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn preproc_tracks_column_mapping_for_macro_expansion() {
        let source = "#define VALUE LongIdentifier\nint32 Answer = VALUE;\n";
        let result = preprocess_source(source, &default_macro_table());
        let line = 1u32;
        assert_eq!(result.map_column(line, 15), 15);
        assert_eq!(result.map_column(line, 16), 15);
        assert_eq!(result.map_column(line, 29), 20);
    }

    #[test]
    fn preproc_expands_function_like_macros() {
        let source = "#define ADD(X, Y) ((X) + (Y))\nint32 Answer = ADD(1, 2);\n";
        let result = preprocess_source(source, &default_macro_table());
        assert!(result.expanded_source.contains("int32 Answer = ((1) + (2));"));
    }

    #[test]
    fn preproc_expands_nested_macro_arguments() {
        let source = "#define VALUE 7\n#define ID(X) X\nint32 Answer = ID(VALUE);\n";
        let result = preprocess_source(source, &default_macro_table());
        assert!(result.expanded_source.contains("int32 Answer = 7;"));
    }

    #[test]
    fn preproc_caches_predefined_macros_per_file_name() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let data_dir = Path::new(manifest_dir).join("data");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_a = format!("predefined_macros_test_{unique}_a.toml");
        let file_b = format!("predefined_macros_test_{unique}_b.toml");
        let path_a = data_dir.join(&file_a);
        let path_b = data_dir.join(&file_b);

        fs::write(&path_a, "[defines]\npredefined = [\"TEST_A=1\"]\n").unwrap();
        fs::write(&path_b, "[defines]\npredefined = [\"TEST_B=2\"]\n").unwrap();

        let table_a = default_macro_table_for_file(&file_a);
        let table_b = default_macro_table_for_file(&file_b);

        assert!(table_a.is_defined("TEST_A"));
        assert_eq!(table_a.value_of("TEST_A"), Some("1"));
        assert!(!table_a.is_defined("TEST_B"));

        assert!(table_b.is_defined("TEST_B"));
        assert_eq!(table_b.value_of("TEST_B"), Some("2"));
        assert!(!table_b.is_defined("TEST_A"));

        let _ = fs::remove_file(path_a);
        let _ = fs::remove_file(path_b);
    }

    #[test]
    fn preproc_loads_include_paths_from_preprocessor_config() {
        let config = default_preprocessor_config_for_file("preprocessor.toml");
        assert!(config
            .include_paths
            .project_search_dirs
            .iter()
            .any(|dir| dir == "Source/${ProjectName}"));
        assert!(config
            .include_paths
            .engine_search_dirs
            .iter()
            .any(|dir| dir == "Engine/Source/Runtime"));
    }

    #[test]
    fn preproc_handles_has_include_with_project_search_dirs() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_has_include_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = root.join("Source/MyGame/Private/Test.cpp");
        let header = root.join("Source/MyGame/Public/MyHeader.h");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::create_dir_all(header.parent().unwrap()).unwrap();
        fs::write(root.join("MyGame.uproject"), "{}").unwrap();
        fs::write(&file, "").unwrap();
        fs::write(&header, "// header").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let source =
            "#if __has_include(\"MyHeader.h\")\nint32 Value = 1;\n#else\nint32 Value = 0;\n#endif\n";
        let result =
            preprocess_source_with_resolver(source, &default_macro_table(), Some(&resolver));
        assert!(result.expanded_source.contains("int32 Value = 1;"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_caches_expanded_output_on_disk() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_cache_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = root.join("Source/MyGame/Private/Test.cpp");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(root.join("MyGame.uproject"), "{}").unwrap();
        fs::write(&file, "#define VALUE 7\nint32 Answer = VALUE;\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let source = "#define VALUE 7\nint32 Answer = VALUE;\n";
        let result = preprocess_source_cached_with_resolver(
            source,
            &default_macro_table(),
            Some(&resolver),
            Some(&file.to_string_lossy()),
        );
        assert!(result.expanded_source.contains("int32 Answer = 7;"));

        let cache_dir = root.join(".ucore").join("preproc");
        let entries = fs::read_dir(&cache_dir)
            .unwrap()
            .flatten()
            .collect::<Vec<_>>();
        assert!(!entries.is_empty());

        let result_again = preprocess_source_cached_with_resolver(
            source,
            &default_macro_table(),
            Some(&resolver),
            Some(&file.to_string_lossy()),
        );
        assert_eq!(result.expanded_source, result_again.expanded_source);

        let _ = fs::remove_dir_all(root);
    }
}
