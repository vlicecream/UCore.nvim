use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

use crate::server::state::{AppState, AssetGraph};
use crate::server::utils::normalize_path_key;
use crate::types::ProgressReporter;
use crate::uasset::UAssetParser;

const DISCOVERY_MAX_DEPTH: usize = 4;
const LOG_EVERY: usize = 1000;
const ASSET_INDEX_VERSION: i32 = 3;
const ASSET_PROGRESS_EVERY: usize = 100;

fn script_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/Script/[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)+").unwrap())
}

fn game_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/Game/[A-Za-z0-9_/]+(?:[.:][A-Za-z0-9_]+)?").unwrap())
}

fn ident_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]{2,}").unwrap())
}

/// Run a targeted asset scan for one Unreal project root.
/// 对一个 Unreal 工程根目录执行定向资产扫描。
pub async fn handle_asset_scan(state: Arc<AppState>, project_root: String) {
    let root_key = normalize_path_key(&project_root);
    let _guard = ActiveAssetScanGuard::new(state.clone(), root_key.clone());

    info!("Starting asset scan: {}", project_root);

    let root = PathBuf::from(project_root.clone());
    let scan_result = tokio::task::spawn_blocking(move || scan_project_assets(&root, None)).await;

    match scan_result {
        Ok(Ok(report)) => {
            info!(
                "Asset scan completed: {} files, {} parsed, {} skipped, {} errors",
                report.total_seen,
                report.parsed.len(),
                report.skipped,
                report.errors
            );

            let graph = build_asset_graph(report.parsed);

            let mut graphs = state.asset_graphs.lock();
            graphs.insert(root_key, graph);
        }

        Ok(Err(err)) => {
            warn!("Asset scan failed for {}: {}", project_root, err);
        }

        Err(join_err) => {
            warn!("Asset scan task failed for {}: {}", project_root, join_err);
        }
    }
}

/// Update one changed asset inside the in-memory graph.
/// 增量更新内存资产图里的单个资产。
pub async fn update_single_asset(state: Arc<AppState>, project_root: &str, file_path: &Path) {
    let root_key = normalize_path_key(project_root);
    let path = file_path.to_path_buf();

    let parse_result = tokio::task::spawn_blocking(move || parse_asset_record(&path)).await;

    match parse_result {
        Ok(Ok(record)) => {
            let mut graphs = state.asset_graphs.lock();

            if let Some(graph) = graphs.get_mut(&root_key) {
                remove_asset_from_graph(graph, &record.asset_path);
                insert_asset_record(graph, record);
                info!("Incremental asset update: {}", file_path.display());
            }
        }

        Ok(Err(err)) => {
            warn!("Failed to update asset {}: {}", file_path.display(), err);
        }

        Err(join_err) => {
            warn!(
                "Incremental asset update task failed for {}: {}",
                file_path.display(),
                join_err
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Scan lifecycle
// -----------------------------------------------------------------------------

/// Guard that clears active_asset_scans when the scan exits.
/// 扫描退出时自动清理 active_asset_scans 标记。
struct ActiveAssetScanGuard {
    state: Arc<AppState>,
    root_key: String,
}

impl ActiveAssetScanGuard {
    /// Create a new guard for one project root.
/// 为某个工程 root 创建扫描保护对象。
    fn new(state: Arc<AppState>, root_key: String) -> Self {
        Self { state, root_key }
    }
}

impl Drop for ActiveAssetScanGuard {
    fn drop(&mut self) {
        let mut active = self.state.active_asset_scans.lock();
        active.remove(&self.root_key);
        info!("Asset scan flag cleared for: {}", self.root_key);
    }
}

/// Return true when the persistent asset index has been initialized at least once.
/// 判断持久化资产索引是否至少初始化过一次。
pub fn asset_index_initialized(conn: &Connection) -> Result<bool> {
    let initialized = conn
        .query_row(
            "SELECT value FROM project_meta WHERE key = 'asset_index_initialized'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    if !matches!(initialized.as_deref(), Some("1")) {
        return Ok(false);
    }

    let version = conn
        .query_row(
            "SELECT value FROM project_meta WHERE key = 'asset_index_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i32>().ok());

    Ok(version == Some(ASSET_INDEX_VERSION))
}

/// Scan project assets and persist the result into the project DB.
/// 扫描项目资产并把结果持久化到项目数据库。
pub fn refresh_asset_index(
    conn: &mut Connection,
    project_root: &Path,
    reporter: Arc<dyn ProgressReporter>,
) -> Result<()> {
    let initialized = asset_index_initialized(conn)?;
    let content_dirs = discover_content_dirs(project_root);
    let asset_files = collect_candidate_assets(&content_dirs);

    if !initialized {
        let report = parse_asset_files(&asset_files, Some(reporter.as_ref()))?;
        reporter.report(
            "asset_index",
            0,
            report.parsed.len().max(1),
            "Persist",
        );
        replace_asset_index(conn, &report.parsed, Some(reporter.as_ref()))?;
        reporter.report(
            "asset_index",
            report.parsed.len().max(1),
            report.parsed.len().max(1),
            "Ready",
        );
        return Ok(());
    }

    let existing = load_existing_asset_mtimes(conn)?;
    let mut changed_files = Vec::new();
    let mut on_disk = HashSet::new();
    let total_seen = asset_files.len().max(1);

    for (index, path) in asset_files.iter().enumerate() {
        let current = index + 1;
        if current == asset_files.len() || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
            reporter.report("asset_index", current, total_seen, "Scan");
        }

        let source_path = normalize_path(path);
        on_disk.insert(source_path.clone());
        let mtime = file_mtime(path);
        if existing.get(&source_path).copied() != Some(mtime) {
            changed_files.push(path.clone());
        }
    }

    let deleted_paths = existing
        .keys()
        .filter(|path| !on_disk.contains(*path))
        .cloned()
        .collect::<Vec<_>>();

    let report = parse_asset_files(&changed_files, None)?;
    let work_total = (deleted_paths.len() + report.parsed.len()).max(1);

    reporter.report("asset_index", 0, work_total, "Persist");
    apply_asset_index_delta(conn, &report.parsed, &deleted_paths, Some(reporter.as_ref()))?;
    reporter.report("asset_index", work_total, work_total, "Ready");
    Ok(())
}

/// Persist a full asset index replacement.
/// 全量替换资产索引。
pub fn replace_asset_index(
    conn: &mut Connection,
    records: &[AssetRecord],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute("DELETE FROM asset_references", [])?;
    tx.execute("DELETE FROM asset_functions", [])?;
    tx.execute("DELETE FROM assets", [])?;

    {
        let mut stmt_asset = tx.prepare(
            "INSERT INTO assets
             (asset_path, asset_key, source_path, source_path_key, parent_class, parent_class_key, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let mut stmt_reference = tx.prepare(
            "INSERT INTO asset_references (asset_path, reference_key) VALUES (?1, ?2)",
        )?;
        let mut stmt_function = tx.prepare(
            "INSERT INTO asset_functions (asset_path, function_key) VALUES (?1, ?2)",
        )?;

        let total = records.len().max(1);
        for (index, record) in records.iter().enumerate() {
            let current = index + 1;
            if let Some(reporter) = reporter {
                if current == total || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
                    reporter.report("asset_index", current, total, "Persist");
                }
            }

            insert_asset_record_db(
                &mut stmt_asset,
                &mut stmt_reference,
                &mut stmt_function,
                record,
            )?;
        }
    }

    write_asset_index_initialized(&tx)?;
    tx.commit()?;
    Ok(())
}

fn apply_asset_index_delta(
    conn: &mut Connection,
    records: &[AssetRecord],
    deleted_paths: &[String],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    let tx = conn.transaction()?;

    let total = (deleted_paths.len() + records.len()).max(1);
    let mut current = 0usize;

    {
        let mut stmt_asset = tx.prepare(
            "INSERT INTO assets
             (asset_path, asset_key, source_path, source_path_key, parent_class, parent_class_key, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let mut stmt_reference = tx.prepare(
            "INSERT INTO asset_references (asset_path, reference_key) VALUES (?1, ?2)",
        )?;
        let mut stmt_function = tx.prepare(
            "INSERT INTO asset_functions (asset_path, function_key) VALUES (?1, ?2)",
        )?;

        for source_path in deleted_paths {
            current += 1;
            if let Some(reporter) = reporter {
                if current == total || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
                    reporter.report("asset_index", current, total, "Persist");
                }
            }
            delete_asset_by_source_path_tx(&tx, source_path)?;
        }

        for record in records {
            current += 1;
            if let Some(reporter) = reporter {
                if current == total || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
                    reporter.report("asset_index", current, total, "Persist");
                }
            }
            delete_asset_by_source_path_tx(&tx, &record.source_path)?;
            insert_asset_record_db(
                &mut stmt_asset,
                &mut stmt_reference,
                &mut stmt_function,
                record,
            )?;
        }
    }

    write_asset_index_initialized(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Upsert one asset file into the persistent asset index.
/// 把单个资产文件写入持久化资产索引。
pub fn upsert_asset_file(conn: &mut Connection, file_path: &Path) -> Result<()> {
    let record = parse_asset_record(file_path)?;
    let tx = conn.unchecked_transaction()?;

    delete_asset_by_source_path_tx(&tx, &record.source_path)?;

    {
        let mut stmt_asset = tx.prepare(
            "INSERT INTO assets
             (asset_path, asset_key, source_path, source_path_key, parent_class, parent_class_key, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let mut stmt_reference = tx.prepare(
            "INSERT INTO asset_references (asset_path, reference_key) VALUES (?1, ?2)",
        )?;
        let mut stmt_function = tx.prepare(
            "INSERT INTO asset_functions (asset_path, function_key) VALUES (?1, ?2)",
        )?;

        insert_asset_record_db(
            &mut stmt_asset,
            &mut stmt_reference,
            &mut stmt_function,
            &record,
        )?;
    }

    write_asset_index_initialized(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Delete one asset file from the persistent asset index.
/// 从持久化资产索引删除单个资产文件。
pub fn delete_asset_file(conn: &mut Connection, file_path: &Path) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    delete_asset_by_source_path_tx(&tx, &normalize_path(file_path))?;
    write_asset_index_initialized(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Full scan report produced by the blocking worker.
/// 阻塞扫描线程返回的完整扫描报告。
pub struct AssetScanReport {
    pub total_seen: usize,
    pub skipped: usize,
    pub errors: usize,
    pub parsed: Vec<AssetRecord>,
    pub error_summary: Vec<AssetParseErrorSummary>,
}

#[derive(Debug, Clone)]
pub struct AssetParseErrorSummary {
    pub reason: String,
    pub count: usize,
    pub sample_path: String,
}

/// Parsed information for one asset file.
/// 单个资产文件解析出来的信息。
#[derive(Debug)]
pub struct AssetRecord {
    pub asset_path: String,
    pub source_path: String,
    pub mtime: i64,
    pub parent_class: Option<String>,
    pub imports: Vec<String>,
    pub functions: Vec<String>,
}

fn load_existing_asset_mtimes(conn: &Connection) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT source_path, mtime FROM assets")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut items = HashMap::new();
    for row in rows {
        let (path, mtime) = row?;
        items.insert(path, mtime);
    }
    Ok(items)
}

/// Scan one Unreal project root and parse selected assets.
/// 扫描一个 Unreal 工程根目录，并解析筛选后的资产。
pub fn scan_project_assets(
    project_root: &Path,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<AssetScanReport> {
    let content_dirs = discover_content_dirs(project_root);
    let asset_files = collect_candidate_assets(&content_dirs);
    parse_asset_files(&asset_files, reporter)
}

fn parse_asset_files(
    asset_files: &[PathBuf],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<AssetScanReport> {
    let total_seen = asset_files.len();
    let processed = AtomicUsize::new(0);
    let reported = AtomicUsize::new(0);
    if let Some(reporter) = reporter {
        reporter.report("asset_index", 0, total_seen.max(1), "Scan");
    }

    let parsed_results = asset_files
        .par_iter()
        .filter_map(|path| {
            let current = processed.fetch_add(1, Ordering::Relaxed) + 1;
            if current > 0 && current % LOG_EVERY == 0 {
                debug!("Asset scan progress: {} files visited", current);
            }
            if let Some(reporter) = reporter {
                let previous = reported.load(Ordering::Relaxed);
                if current == total_seen
                    || current == 1
                    || current % ASSET_PROGRESS_EVERY == 0
                    || (current > previous
                        && reported
                            .compare_exchange(previous, current, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok())
                {
                    reporter.report("asset_index", current, total_seen.max(1), "Scan");
                }
            }

            match parse_asset_record(path) {
                Ok(record) => Some(Ok(record)),
                Err(err) => Some(Err((path.clone(), err))),
            }
        })
        .collect::<Vec<_>>();

    let mut parsed = Vec::new();
    let mut errors = 0usize;
    let mut error_buckets = HashMap::<String, (usize, String)>::new();

    for item in parsed_results {
        match item {
            Ok(record) => parsed.push(record),
            Err((path, err)) => {
                errors += 1;
                let reason = err.to_string();
                let entry = error_buckets
                    .entry(reason)
                    .or_insert_with(|| (0usize, path.display().to_string()));
                entry.0 += 1;
            }
        }
    }

    let mut error_summary = error_buckets
        .into_iter()
        .map(|(reason, (count, sample_path))| AssetParseErrorSummary {
            reason,
            count,
            sample_path,
        })
        .collect::<Vec<_>>();
    error_summary.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.reason.cmp(&right.reason))
    });

    if !error_summary.is_empty() {
        let summary = error_summary
            .iter()
            .take(5)
            .map(|item| {
                format!(
                    "{}x {} (sample: {})",
                    item.count, item.reason, item.sample_path
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");

        warn!(
            "Asset scan parse summary: {} total errors across {} reasons. Top failures: {}",
            errors,
            error_summary.len(),
            summary,
        );
    }

    Ok(AssetScanReport {
        total_seen,
        skipped: 0,
        errors,
        parsed,
        error_summary,
    })
}

/// Find Content directories under the project root.
/// 在工程根目录下查找 Content 目录。
fn discover_content_dirs(project_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(false)
        .git_ignore(false)
        .follow_links(true)
        .max_depth(Some(DISCOVERY_MAX_DEPTH))
        .filter_entry(|entry| !is_ignored_dir(entry.path()))
        .build();

    for entry in walker.filter_map(|entry| entry.ok()) {
        let path = entry.path();

        if entry.file_type().map_or(false, |ty| ty.is_dir())
            && entry.file_name().to_string_lossy().eq_ignore_ascii_case("Content")
        {
            let normalized = normalize_path(path);
            if seen.insert(normalized) {
                dirs.push(path.to_path_buf());
            }
        }
    }

    dirs
}

/// Collect .uasset/.umap files from Content directories.
/// 从 Content 目录收集 .uasset/.umap 文件。
fn collect_candidate_assets(content_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();

    for content_dir in content_dirs {
        let walker = ignore::WalkBuilder::new(content_dir)
            .hidden(false)
            .git_ignore(false)
            .follow_links(true)
            .filter_entry(|entry| !is_ignored_dir(entry.path()))
            .build();

        for entry in walker.filter_map(|entry| entry.ok()) {
            let path = entry.path();

            if !entry.file_type().map_or(false, |ty| ty.is_file()) {
                continue;
            }

            if !is_unreal_asset_file(path) {
                continue;
            }

            let normalized = normalize_path(path);
            if seen.insert(normalized) {
                files.push(path.to_path_buf());
            }
        }
    }

    files
}

/// Parse one asset file into an AssetRecord.
/// 把单个资产文件解析成 AssetRecord。
pub fn parse_asset_record(path: &Path) -> Result<AssetRecord> {
    let path = path.to_path_buf();
    let parse_path = path.clone();

    let parse_result = std::panic::catch_unwind(move || {
        let mut parser = UAssetParser::new();
        parser
            .parse(&parse_path)
            .map(|_| AssetRecord {
                asset_path: to_asset_path(&parse_path),
                source_path: normalize_path(&parse_path),
                mtime: file_mtime(&parse_path),
                parent_class: parser.parent_class,
                imports: parser.imports,
                functions: parser.functions,
            })
    });

    match parse_result {
        Ok(Ok(record)) => Ok(record),
        Ok(Err(err)) => fallback_parse_asset_record(&path).or(Err(err)),
        Err(_) => fallback_parse_asset_record(&path).or_else(|_| Err(anyhow::anyhow!("panic while parsing asset"))),
    }
}

fn fallback_parse_asset_record(path: &Path) -> Result<AssetRecord> {
    let bytes = std::fs::read(path)?;
    let strings = extract_ascii_strings(&bytes, 4);
    let parent_class = detect_parent_class_from_strings(&strings);
    let asset_path = to_asset_path(path);
    let mut imports = HashSet::new();
    let mut functions = HashSet::new();

    for text in &strings {
        for capture in script_path_re().find_iter(text) {
            let value = capture.as_str();
            if should_index_import_path(value, &asset_path) {
                imports.insert(value.to_string());
            }
        }

        for capture in game_path_re().find_iter(text) {
            let value = capture.as_str();
            if should_index_import_path(value, &asset_path) {
                imports.insert(value.to_string());
            }
        }

        for capture in ident_re().find_iter(text) {
            let token = capture.as_str();
            if looks_like_unreal_type_token(token) {
                imports.insert(token.to_string());
            }
            if looks_like_blueprint_function_token(token) {
                functions.insert(token.to_string());
            }
        }
    }

    if imports.is_empty() && functions.is_empty() && parent_class.is_none() {
        return Err(anyhow::anyhow!("asset fallback found no indexable symbols"));
    }

    let mut imports = imports.into_iter().collect::<Vec<_>>();
    imports.sort();
    imports.dedup();

    let mut functions = functions.into_iter().collect::<Vec<_>>();
    functions.sort();
    functions.dedup();

    Ok(AssetRecord {
        asset_path,
        source_path: normalize_path(path),
        mtime: file_mtime(path),
        parent_class,
        imports,
        functions,
    })
}

// -----------------------------------------------------------------------------
// Graph building
// -----------------------------------------------------------------------------

/// Build a complete AssetGraph from parsed records.
/// 根据解析结果构建完整 AssetGraph。
fn build_asset_graph(records: Vec<AssetRecord>) -> AssetGraph {
    let mut graph = AssetGraph::default();

    for record in records {
        insert_asset_record(&mut graph, record);
    }

    graph
}

/// Insert one parsed asset record into the graph.
/// 把单个资产记录写入资产图。
fn insert_asset_record(graph: &mut AssetGraph, record: AssetRecord) {
    let asset_key: Arc<str> = record.asset_path.to_ascii_lowercase().into();

    if let Some(parent) = record.parent_class {
        graph
            .derived
            .entry(parent.to_ascii_lowercase().into())
            .or_default()
            .insert(asset_key.clone());
    }

    for import in record.imports {
        graph
            .references
            .entry(import.to_ascii_lowercase().into())
            .or_default()
            .insert(asset_key.clone());
    }

    for function in record.functions {
        graph
            .functions
            .entry(function.to_ascii_lowercase().into())
            .or_default()
            .insert(asset_key.clone());
    }
}

/// Remove an asset from all graph indexes before incremental reinsert.
/// 增量更新前，先从所有索引里移除旧的资产记录。
fn remove_asset_from_graph(graph: &mut AssetGraph, asset_path: &str) {
    let asset_key = asset_path.to_ascii_lowercase();

    for assets in graph.derived.values_mut() {
        assets.retain(|item| item.as_ref() != asset_key);
    }

    for assets in graph.references.values_mut() {
        assets.retain(|item| item.as_ref() != asset_key);
    }

    for assets in graph.functions.values_mut() {
        assets.retain(|item| item.as_ref() != asset_key);
    }
}

/// Query all indexed assets from the persistent DB.
/// 从持久化数据库获取全部已索引资产。
pub fn get_assets(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare("SELECT asset_path FROM assets ORDER BY asset_path COLLATE NOCASE")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let assets = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!(assets))
}

/// Query asset usages from the persistent DB.
/// 从持久化数据库查询资产引用和派生关系。
pub fn get_asset_usages(conn: &Connection, asset_path: &str) -> Result<Value> {
    let names = make_asset_lookup_names(asset_path);
    let mut references = HashSet::new();
    let mut function_references = HashSet::new();
    let mut derived = HashSet::new();

    let mut stmt_ref = conn.prepare(
        "SELECT DISTINCT asset_path
         FROM asset_references
         WHERE reference_key = ?1 OR reference_key LIKE ?2",
    )?;
    let mut stmt_func = conn.prepare(
        "SELECT DISTINCT asset_path
         FROM asset_functions
         WHERE function_key = ?1 OR function_key LIKE ?2 OR function_key LIKE ?3",
    )?;
    let mut stmt_derived = conn.prepare(
        "SELECT DISTINCT asset_path
         FROM assets
         WHERE parent_class_key = ?1 OR parent_class_key LIKE ?2",
    )?;

    for name in names {
        collect_asset_rows(
            &mut stmt_ref,
            params![name, format!("%.{}", name)],
            &mut references,
        )?;
        collect_asset_rows(
            &mut stmt_func,
            params![name, format!("%.{}", name), format!("%:{}", name)],
            &mut function_references,
        )?;
        collect_asset_rows(
            &mut stmt_derived,
            params![name, format!("%.{}", name)],
            &mut derived,
        )?;
    }

    Ok(json!({
        "status": "ready",
        "references": sorted_strings(references),
        "function_references": sorted_strings(function_references),
        "derived": sorted_strings(derived),
    }))
}

/// Merge Blueprint-derived assets into a class query result from the persistent DB.
/// 从持久化数据库把蓝图派生资产合并到 class 查询结果。
pub fn merge_derived_classes(conn: &Connection, base_class: &str, results: &mut Vec<Value>) -> Result<()> {
    let names = make_asset_lookup_names(base_class);
    let mut stmt = conn.prepare(
        "SELECT DISTINCT asset_path
         FROM assets
         WHERE parent_class_key = ?1 OR parent_class_key LIKE ?2",
    )?;

    for name in names {
        let mut rows = stmt.query(params![name, format!("%.{}", name)])?;
        while let Some(row) = rows.next()? {
            let asset: String = row.get(0)?;
            let exists = results.iter().any(|item| {
                item["path"]
                    .as_str()
                    .map(|path| path.eq_ignore_ascii_case(&asset))
                    .unwrap_or(false)
            });

            if !exists {
                results.push(json!({
                    "name": asset.rsplit('/').next().unwrap_or(asset.as_str()),
                    "path": asset,
                    "symbol_type": "uasset",
                }));
            }
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Filters and path helpers
// -----------------------------------------------------------------------------

/// Return true if a directory should be ignored during asset scan.
/// 判断扫描资产时是否应该跳过某个目录。
fn is_ignored_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    matches!(
        name,
        "Intermediate" | "Binaries" | "Build" | "Saved" | ".git" | ".vs" | "DerivedDataCache"
    )
}

/// Return true for .uasset and .umap files.
/// 判断是否是 Unreal 资产文件。
fn is_unreal_asset_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if ext == "uasset" || ext == "umap"
    )
}

/// Convert a filesystem path to Unreal asset path.
/// 把文件系统路径转换成 Unreal 资产路径。
pub fn to_asset_path(path: &Path) -> String {
    let normalized = normalize_path(path);

    if let Some(index) = normalized.find("/Content/") {
        let sub_path = &normalized[index + "/Content/".len()..];
        let without_ext = sub_path
            .rsplit_once('.')
            .map(|(base, _)| base)
            .unwrap_or(sub_path);

        return format!("/Game/{}", without_ext);
    }

    normalized
}

fn insert_asset_record_db(
    stmt_asset: &mut rusqlite::Statement,
    stmt_reference: &mut rusqlite::Statement,
    stmt_function: &mut rusqlite::Statement,
    record: &AssetRecord,
) -> Result<()> {
    let asset_key = record.asset_path.to_ascii_lowercase();
    let source_path_key = record.source_path.to_ascii_lowercase();
    let parent_class_key = record.parent_class.as_ref().map(|value| value.to_ascii_lowercase());

    stmt_asset.execute(params![
        record.asset_path.as_str(),
        asset_key,
        record.source_path.as_str(),
        source_path_key,
        record.parent_class.as_deref(),
        parent_class_key.as_deref(),
        record.mtime,
    ])?;

    let mut seen_references = HashSet::new();
    for import in &record.imports {
        let key = import.to_ascii_lowercase();
        if seen_references.insert(key.clone()) {
            stmt_reference.execute(params![record.asset_path.as_str(), key])?;
        }
    }

    let mut seen_functions = HashSet::new();
    for function in &record.functions {
        let key = function.to_ascii_lowercase();
        if seen_functions.insert(key.clone()) {
            stmt_function.execute(params![record.asset_path.as_str(), key])?;
        }
    }

    Ok(())
}

fn delete_asset_by_source_path_tx(tx: &rusqlite::Transaction, source_path: &str) -> Result<()> {
    let source_path_key = source_path.to_ascii_lowercase();

    tx.execute(
        "DELETE FROM asset_references
         WHERE asset_path IN (
             SELECT asset_path FROM assets WHERE source_path_key = ?1
         )",
        [source_path_key.as_str()],
    )?;
    tx.execute(
        "DELETE FROM asset_functions
         WHERE asset_path IN (
             SELECT asset_path FROM assets WHERE source_path_key = ?1
         )",
        [source_path_key.as_str()],
    )?;
    tx.execute(
        "DELETE FROM assets WHERE source_path_key = ?1",
        [source_path_key.as_str()],
    )?;

    Ok(())
}

fn write_asset_index_initialized(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO project_meta (key, value)
         VALUES ('asset_index_initialized', '1')",
        [],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO project_meta (key, value)
         VALUES ('asset_index_version', ?1)",
        [ASSET_INDEX_VERSION.to_string()],
    )?;
    Ok(())
}

fn collect_asset_rows<P: rusqlite::Params>(
    stmt: &mut rusqlite::Statement,
    params: P,
    target: &mut HashSet<String>,
) -> Result<()> {
    let mut rows = stmt.query(params)?;
    while let Some(row) = rows.next()? {
        target.insert(row.get::<_, String>(0)?);
    }
    Ok(())
}

fn extract_ascii_strings(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = Vec::new();

    for byte in bytes {
        if (32..=126).contains(byte) {
            current.push(*byte);
            continue;
        }

        if current.len() >= min_len {
            values.push(String::from_utf8_lossy(&current).to_string());
        }
        current.clear();
    }

    if current.len() >= min_len {
        values.push(String::from_utf8_lossy(&current).to_string());
    }

    values
}

fn detect_parent_class_from_strings(strings: &[String]) -> Option<String> {
    let mut best: Option<(i32, String)> = None;

    for text in strings {
        for capture in script_path_re().find_iter(text) {
            let candidate = capture.as_str();
            if !looks_like_parent_class_candidate(candidate) {
                continue;
            }

            let score = parent_class_score(candidate);
            let replace = best
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true);

            if replace {
                best = Some((score, candidate.to_string()));
            }
        }
    }

    best.map(|(_, value)| value)
}

fn looks_like_parent_class_candidate(candidate: &str) -> bool {
    if !candidate.starts_with("/Script/") || !candidate.contains('.') {
        return false;
    }

    let class_name = candidate.rsplit('.').next().unwrap_or(candidate);
    !matches!(
        class_name,
        "Class" | "Package" | "MetaData" | "BlueprintGeneratedClass" | "Default__Object"
    )
}

fn parent_class_score(candidate: &str) -> i32 {
    let module = candidate
        .strip_prefix("/Script/")
        .and_then(|value| value.split('.').next())
        .unwrap_or("");

    if !matches!(module, "CoreUObject" | "Engine" | "UnrealEd") {
        return 100;
    }

    if module == "Engine" {
        return 20;
    }

    10
}

fn should_index_import_path(value: &str, asset_path: &str) -> bool {
    if value.eq_ignore_ascii_case(asset_path) {
        return false;
    }

    !value.ends_with(".Class")
}

fn looks_like_blueprint_function_token(token: &str) -> bool {
    if token.len() < 3 || token.len() > 80 {
        return false;
    }

    if token.ends_with("_C")
        || token.ends_with("_GEN_VARIABLE")
        || token.starts_with("Default__")
        || token.starts_with("ExecuteUbergraph_")
        || token.starts_with("K2Node_")
        || token.starts_with("SKEL_")
        || token.starts_with("REINST_")
    {
        return false;
    }

    if token.contains("__") {
        return false;
    }

    let has_upper = token.chars().any(|ch| ch.is_ascii_uppercase());
    let has_lower = token.chars().any(|ch| ch.is_ascii_lowercase());
    has_upper && has_lower
}

fn looks_like_unreal_type_token(token: &str) -> bool {
    if token.len() < 3 || token.len() > 100 {
        return false;
    }

    if token.ends_with("_C")
        || token.ends_with("_GEN_VARIABLE")
        || token.starts_with("Default__")
        || token.starts_with("SKEL_")
        || token.starts_with("REINST_")
    {
        return false;
    }

    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let Some(second) = chars.next() else {
        return false;
    };

    if !matches!(first, 'A' | 'U' | 'F' | 'S' | 'E' | 'T' | 'I') || !second.is_ascii_uppercase() {
        return false;
    }

    token.chars().any(|ch| ch.is_ascii_lowercase())
}

fn make_asset_lookup_names(input: &str) -> Vec<String> {
    let class_name = if input.starts_with("/Script/") {
        input.rsplit('.').next().unwrap_or(input)
    } else {
        input
    };

    let mut names = vec![class_name.to_ascii_lowercase()];
    let prefixes = ['a', 'u', 'f', 'e', 't', 's'];
    let mut chars = class_name.chars();

    if let (Some(first), Some(second)) = (chars.next(), chars.next()) {
        if prefixes.contains(&first.to_ascii_lowercase()) && second.is_uppercase() {
            names.push(class_name[first.len_utf8()..].to_ascii_lowercase());
        }
    }

    names.sort();
    names.dedup();
    names
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut items = values.into_iter().collect::<Vec<_>>();
    items.sort();
    items
}

fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Normalize a path to slash-separated string.
/// 把路径统一成斜杠分隔字符串。
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").replace("//", "/")
}

#[cfg(test)]
mod tests {
    use super::{
        detect_parent_class_from_strings, extract_ascii_strings, looks_like_blueprint_function_token,
        looks_like_unreal_type_token,
        make_asset_lookup_names,
    };

    #[test]
    fn lookup_names_include_prefixed_and_unprefixed_class_names() {
        let names = make_asset_lookup_names("USInventoryManagerComponent");
        assert_eq!(names, vec!["sinventorymanagercomponent", "usinventorymanagercomponent"]);
    }

    #[test]
    fn lookup_names_handle_script_paths() {
        let names = make_asset_lookup_names("/Script/SimpleBeta.SHero");
        assert_eq!(names, vec!["hero", "shero"]);
    }

    #[test]
    fn fallback_parent_class_prefers_project_script_class() {
        let strings = vec![
            "/Script/Engine.BlueprintGeneratedClass".to_string(),
            "/Script/CoreUObject.Class".to_string(),
            "/Script/SimpleBeta.SHero".to_string(),
        ];
        assert_eq!(
            detect_parent_class_from_strings(&strings),
            Some("/Script/SimpleBeta.SHero".to_string())
        );
    }

    #[test]
    fn fallback_ascii_string_extraction_keeps_printable_sequences() {
        let strings = extract_ascii_strings(b"\0/Game/Test/BP_Hero\0SHero\0", 4);
        assert_eq!(strings, vec!["/Game/Test/BP_Hero", "SHero"]);
    }

    #[test]
    fn fallback_function_token_filters_noise() {
        assert!(looks_like_blueprint_function_token("RefreshCurrency"));
        assert!(!looks_like_blueprint_function_token("ExecuteUbergraph_BP_Hero"));
        assert!(!looks_like_blueprint_function_token("BP_Hero_C"));
    }

    #[test]
    fn fallback_type_token_accepts_unreal_style_class_names() {
        assert!(looks_like_unreal_type_token("SInventoryManagerComponent"));
        assert!(looks_like_unreal_type_token("UWeaponForgeMain"));
        assert!(!looks_like_unreal_type_token("RefreshCurrency"));
    }
}
