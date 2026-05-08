use anyhow::Result;
use rayon::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::server::state::{AppState, AssetGraph};
use crate::server::utils::normalize_path_key;
use crate::types::ProgressReporter;
use crate::uasset::UAssetParser;

const DISCOVERY_MAX_DEPTH: usize = 4;
const LOG_EVERY: usize = 1000;

/// Run a targeted asset scan for one Unreal project root.
/// 对一个 Unreal 工程根目录执行定向资产扫描。
pub async fn handle_asset_scan(state: Arc<AppState>, project_root: String) {
    let root_key = normalize_path_key(&project_root);
    let _guard = ActiveAssetScanGuard::new(state.clone(), root_key.clone());

    info!("Starting asset scan: {}", project_root);

    let root = PathBuf::from(project_root.clone());
    let scan_result = tokio::task::spawn_blocking(move || scan_project_assets(&root)).await;

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
    let value = conn
        .query_row(
            "SELECT value FROM project_meta WHERE key = 'asset_index_initialized'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    Ok(matches!(value.as_deref(), Some("1")))
}

/// Scan project assets and persist the result into the project DB.
/// 扫描项目资产并把结果持久化到项目数据库。
pub fn refresh_asset_index(
    conn: &mut Connection,
    project_root: &Path,
    reporter: Arc<dyn ProgressReporter>,
) -> Result<()> {
    reporter.report("asset_index", 0, 100, "Scanning Unreal assets...");
    let report = scan_project_assets(project_root)?;

    reporter.report(
        "asset_index",
        70,
        100,
        &format!(
            "Persisting asset index ({} assets, {} errors)...",
            report.parsed.len(),
            report.errors
        ),
    );

    replace_asset_index(conn, &report.parsed)?;
    reporter.report("asset_index", 100, 100, "Asset index ready.");
    Ok(())
}

/// Persist a full asset index replacement.
/// 全量替换资产索引。
pub fn replace_asset_index(conn: &mut Connection, records: &[AssetRecord]) -> Result<()> {
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

        for record in records {
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

/// Scan one Unreal project root and parse selected assets.
/// 扫描一个 Unreal 工程根目录，并解析筛选后的资产。
pub fn scan_project_assets(project_root: &Path) -> Result<AssetScanReport> {
    let content_dirs = discover_content_dirs(project_root);
    let asset_files = collect_candidate_assets(&content_dirs);

    let total_seen = asset_files.len();

    let parsed_results = asset_files
        .par_iter()
        .enumerate()
        .filter_map(|(index, path)| {
            if index > 0 && index % LOG_EVERY == 0 {
                debug!("Asset scan progress: {} files visited", index);
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
            "Asset scan parse summary for {}: {} total errors across {} reasons. Top failures: {}",
            project_root.display(),
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

/// Collect important .uasset/.umap files from Content directories.
/// 从 Content 目录收集重要的 .uasset/.umap 文件。
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

            if !is_important_asset(path) {
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

    let parse_result = std::panic::catch_unwind(move || {
        let mut parser = UAssetParser::new();
        parser
            .parse(&path)
            .map(|_| AssetRecord {
                asset_path: to_asset_path(&path),
                source_path: normalize_path(&path),
                mtime: file_mtime(&path),
                parent_class: parser.parent_class,
                imports: parser.imports,
                functions: parser.functions,
            })
    });

    match parse_result {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("panic while parsing asset")),
    }
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
            &mut references,
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

/// Return true for assets that are worth parsing for navigation/search.
/// 判断资产是否值得解析，用于导航和搜索。
fn is_important_asset(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "umap" {
        return true;
    }

    let filename = path.file_name().and_then(|name| name.to_str()).unwrap_or("");

    filename.starts_with("BP_")
        || filename.starts_with("ABP_")
        || filename.starts_with("WBP_")
        || filename.starts_with("AM_")
        || filename.starts_with("DA_")
        || filename.starts_with("DT_")
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
