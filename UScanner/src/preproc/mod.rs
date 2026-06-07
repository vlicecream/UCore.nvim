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
const MAX_INCLUDE_FINGERPRINT_DEPTH: usize = 32;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LineOrigin {
    pub file_path: Option<String>,
    pub line: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MappedPosition {
    pub file_path: Option<String>,
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PreprocessResult {
    pub expanded_source: String,
    pub inactive_lines: HashSet<u32>,
    #[serde(default)]
    pub line_column_maps: Vec<Vec<u32>>,
    #[serde(default)]
    pub line_origins: Vec<LineOrigin>,
}

impl PreprocessResult {
    pub fn map_column(&self, line: u32, expanded_column: u32) -> u32 {
        let Some(columns) = self.line_column_maps.get(line as usize) else {
            return expanded_column;
        };
        let index = expanded_column.min(columns.len().saturating_sub(1) as u32) as usize;
        columns.get(index).copied().unwrap_or(expanded_column)
    }

    pub fn map_position(&self, line: u32, expanded_column: u32) -> MappedPosition {
        let origin = self.line_origins.get(line as usize);
        MappedPosition {
            file_path: origin.and_then(|origin| origin.file_path.clone()),
            line: origin.map(|origin| origin.line).unwrap_or(line),
            character: self.map_column(line, expanded_column),
        }
    }

    fn ensure_line_origins(&mut self, current_file: Option<&str>) {
        if self.line_origins.is_empty() {
            self.line_origins = (0..self.line_column_maps.len())
                .map(|line| LineOrigin {
                    file_path: current_file.map(normalize_path_string),
                    line: line as u32,
                })
                .collect();
            return;
        }

        while self.line_origins.len() < self.line_column_maps.len() {
            let line = self.line_origins.len() as u32;
            self.line_origins.push(LineOrigin {
                file_path: current_file.map(normalize_path_string),
                line,
            });
        }
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

#[derive(Clone, Debug, Deserialize, Default)]
struct IncludeRootsFile {
    #[serde(default)]
    project: IncludeRootsSection,
    #[serde(default)]
    engine: IncludeRootsSection,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct IncludeRootsSection {
    #[serde(default)]
    search_dirs: Vec<String>,
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
        self.resolve_include_path(include).is_some()
    }

    pub fn resolve_include_path(&self, include: &str) -> Option<PathBuf> {
        let normalized = normalize_include_operand(include);
        if normalized.is_empty() {
            return None;
        }

        let include_path = PathBuf::from(&normalized);
        if include_path.is_absolute() {
            return include_path.is_file().then_some(include_path);
        }

        if let Some(current_dir) = self.current_file.as_deref().and_then(Path::parent) {
            let candidate = current_dir.join(&include_path);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        self.search_roots()
            .into_iter()
            .find_map(|root| root.exists().then(|| find_include_under_root(&root, &normalized)).flatten())
    }

    pub fn current_file_path(&self) -> Option<&Path> {
        self.current_file.as_deref()
    }

    pub fn for_included_file(&self, current_file: PathBuf) -> Self {
        Self {
            current_file: Some(current_file),
            project_root: self.project_root.clone(),
            engine_root: self.engine_root.clone(),
            project_search_dirs: self.project_search_dirs.clone(),
            engine_search_dirs: self.engine_search_dirs.clone(),
        }
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
    let Some(cache_path) = preprocess_cache_path(source, base_macros, include_resolver, current_file) else {
        return expand::preprocess_source_with_resolver(source, base_macros, include_resolver);
    };

    if let Ok(text) = fs::read_to_string(&cache_path) {
        if let Ok(mut cached) = serde_json::from_str::<PreprocessResult>(&text) {
            cached.ensure_line_origins(current_file);
            return cached;
        }
    }

    let mut result = expand::preprocess_source_with_resolver(source, base_macros, include_resolver);
    result.ensure_line_origins(current_file);
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
    let mut parsed = parse_preprocessor_config_file(&text).unwrap_or_default();
    merge_include_roots(&mut parsed);
    parsed
}

fn preprocess_cache_path(
    source: &str,
    base_macros: &MacroTable,
    include_resolver: Option<&IncludeResolver>,
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
    if let Some(resolver) = include_resolver {
        let mut visited = HashSet::new();
        let mut macros = base_macros.clone();
        hash_include_graph(source, &mut macros, resolver, 0, &mut visited, &mut hasher);
    }
    let file_name = format!("{}.json", hasher.finalize().to_hex());
    Some(project_root.join(".ucore").join("preproc").join(file_name))
}

fn hash_include_graph(
    source: &str,
    macros: &mut MacroTable,
    resolver: &IncludeResolver,
    depth: usize,
    visited: &mut HashSet<String>,
    hasher: &mut blake3::Hasher,
) {
    if depth >= MAX_INCLUDE_FINGERPRINT_DEPTH {
        return;
    }

    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(directive) = tokenizer::parse_directive(trimmed) else {
            continue;
        };
        if directive.name == "define" {
            macros.define_from_directive(directive.body);
            continue;
        }
        if directive.name == "undef" {
            macros.undefine(directive.body.trim());
            continue;
        }
        if directive.name != "include" {
            continue;
        }
        let include = expand_include_operand(directive.body.trim(), macros);
        let Some(path) = resolver.resolve_include_path(&include) else {
            continue;
        };
        let key = path
            .canonicalize()
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .replace('\\', "/");
        if !visited.insert(key.clone()) {
            continue;
        }

        hasher.update(key.as_bytes());
        let include_mtime = fs::metadata(&path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs())
            .unwrap_or(0);
        hasher.update(&include_mtime.to_le_bytes());

        if let Ok(include_source) = fs::read_to_string(&path) {
            let child_resolver = resolver.for_included_file(path);
            let mut child_macros = macros.clone();
            hash_include_graph(
                &include_source,
                &mut child_macros,
                &child_resolver,
                depth + 1,
                visited,
                hasher,
            );
        }
    }
}

fn merge_include_roots(config: &mut PreprocessorConfigFile) {
    let roots = parse_include_roots_file(include_str!("../../data/include_roots.toml"));

    for dir in roots.project_search_dirs {
        if !config.include_paths.project_search_dirs.iter().any(|existing| existing == &dir) {
            config.include_paths.project_search_dirs.push(dir);
        }
    }

    for dir in roots.engine_search_dirs {
        if !config.include_paths.engine_search_dirs.iter().any(|existing| existing == &dir) {
            config.include_paths.engine_search_dirs.push(dir);
        }
    }
}

fn parse_preprocessor_config_file(text: &str) -> Option<PreprocessorConfigFile> {
    toml::from_str(text)
        .ok()
        .or_else(|| {
            let roots = parse_include_roots_file(text);
            (!roots.project_search_dirs.is_empty() || !roots.engine_search_dirs.is_empty()).then_some(
                PreprocessorConfigFile {
                    defines: PredefinedDefines::default(),
                    include_paths: IncludePathConfig {
                        project_search_dirs: roots.project_search_dirs,
                        engine_search_dirs: roots.engine_search_dirs,
                    },
                },
            )
        })
}

struct ParsedIncludeRoots {
    project_search_dirs: Vec<String>,
    engine_search_dirs: Vec<String>,
}

fn parse_include_roots_file(text: &str) -> ParsedIncludeRoots {
    let roots = toml::from_str::<IncludeRootsFile>(text).unwrap_or_default();
    ParsedIncludeRoots {
        project_search_dirs: roots.project.search_dirs,
        engine_search_dirs: roots.engine.search_dirs,
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

pub(crate) fn expand_include_operand(include: &str, macros: &MacroTable) -> String {
    macros.expand_line(include.trim()).trim().to_string()
}

fn normalize_path_string(path: &str) -> String {
    path.replace('\\', "/")
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

fn find_include_under_root(root: &Path, include: &str) -> Option<PathBuf> {
    let direct = root.join(include);
    if direct.is_file() {
        return Some(direct);
    }

    let include_suffix = include.replace('\\', "/");
    let target_name = Path::new(&include_suffix)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if target_name.is_empty() {
        return None;
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
        .find_map(|entry| {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if file_name != target_name {
                return None;
            }

            let relative = path
                .strip_prefix(root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            relative
                .ends_with(&include_suffix)
                .then(|| path.to_path_buf())
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
    fn preproc_inlines_include_files_and_tracks_line_origins() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_include_inline_{}",
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
        fs::write(&file, "#include \"MyHeader.h\"\nint32 TestValue();\n").unwrap();
        fs::write(&header, "int32 HeaderValue();\nint32 HeaderValue2();\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#include \"MyHeader.h\"\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );
        let header_path = header.to_string_lossy().replace('\\', "/");
        let file_path = file.to_string_lossy().replace('\\', "/");

        assert!(result.expanded_source.contains("int32 HeaderValue();"));
        assert!(result.expanded_source.contains("int32 HeaderValue2();"));

        let included = result.map_position(1, 0);
        assert_eq!(included.file_path.as_deref(), Some(header_path.as_str()));
        assert_eq!(included.line, 0);

        let source = result.map_position(3, 0);
        assert_eq!(source.file_path.as_deref(), Some(file_path.as_str()));
        assert_eq!(source.line, 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_inlines_include_files_via_macro_operand() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_include_macro_{}",
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
        fs::write(
            &file,
            "#define HEADER_FILE \"MyHeader.h\"\n#include HEADER_FILE\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(&header, "int32 HeaderValue();\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#define HEADER_FILE \"MyHeader.h\"\n#include HEADER_FILE\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert!(result.expanded_source.contains("int32 HeaderValue();"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_inlines_include_files_via_function_macro_operand() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_include_fn_macro_{}",
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
        fs::write(
            &file,
            "#define HEADER_FILE() \"MyHeader.h\"\n#include HEADER_FILE()\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(&header, "int32 HeaderValue();\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#define HEADER_FILE() \"MyHeader.h\"\n#include HEADER_FILE()\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert!(result.expanded_source.contains("int32 HeaderValue();"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_honors_pragma_once_for_repeated_includes() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_pragma_once_{}",
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
        fs::write(
            &file,
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(&header, "#pragma once\nint32 HeaderValue();\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert_eq!(result.expanded_source.matches("int32 HeaderValue();").count(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_honors_ifndef_include_guard_for_repeated_includes() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_include_guard_{}",
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
        fs::write(
            &file,
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(
            &header,
            "#ifndef MY_HEADER_H\n#define MY_HEADER_H\nint32 HeaderValue();\n#endif\n",
        )
        .unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert_eq!(result.expanded_source.matches("int32 HeaderValue();").count(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_honors_if_defined_include_guard_for_repeated_includes() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_if_defined_guard_{}",
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
        fs::write(
            &file,
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(
            &header,
            "#if !defined(MY_HEADER_H)\n#define MY_HEADER_H\nint32 HeaderValue();\n#endif\n",
        )
        .unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert_eq!(result.expanded_source.matches("int32 HeaderValue();").count(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preproc_honors_if_defined_without_parens_include_guard_for_repeated_includes() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_if_defined_no_parens_guard_{}",
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
        fs::write(
            &file,
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
        )
        .unwrap();
        fs::write(
            &header,
            "#if !defined MY_HEADER_H\n#define MY_HEADER_H\nint32 HeaderValue();\n#endif\n",
        )
        .unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let result = preprocess_source_with_resolver(
            "#include \"MyHeader.h\"\n#include \"MyHeader.h\"\nint32 TestValue();\n",
            &default_macro_table(),
            Some(&resolver),
        );

        assert_eq!(result.expanded_source.matches("int32 HeaderValue();").count(), 1);

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

    #[test]
    fn preproc_cache_fingerprint_changes_when_included_header_changes() {
        let root = std::env::temp_dir().join(format!(
            "ucore_preproc_include_cache_{}",
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
        fs::write(&file, "#include \"MyHeader.h\"\n").unwrap();
        fs::write(&header, "int32 HeaderValue();\n").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let first = preprocess_source_cached_with_resolver(
            "#include \"MyHeader.h\"\n",
            &default_macro_table(),
            Some(&resolver),
            Some(&file.to_string_lossy()),
        );
        assert!(first.expanded_source.contains("HeaderValue"));

        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&header, "float HeaderValue();\n").unwrap();

        let second = preprocess_source_cached_with_resolver(
            "#include \"MyHeader.h\"\n",
            &default_macro_table(),
            Some(&resolver),
            Some(&file.to_string_lossy()),
        );
        assert!(second.expanded_source.contains("float HeaderValue();"));

        let _ = fs::remove_dir_all(root);
    }
}
