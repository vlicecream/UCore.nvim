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
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::db::asset as asset_db;
use crate::db::text;
use crate::runtime_index::{self, AssetRuntimeIndex};
use crate::server::state::{AppState, AssetGraph};
use crate::server::utils::normalize_path_key;
use crate::types::ProgressReporter;
use crate::uasset::{sniff_top_level_asset_class, UAssetParser};

const DISCOVERY_MAX_DEPTH: usize = 4;
const LOG_EVERY: usize = 1000;
const ASSET_INDEX_VERSION: i32 = 4;
const ASSET_PROGRESS_EVERY: usize = 100;
const ASSET_BULK_BUSY_TIMEOUT: Duration = Duration::from_millis(60_000);
const LOGICAL_ASSET_CLASS_SUFFIXES: &[&str] = &[
    "Blueprint",
    "WidgetBlueprint",
    "AnimBlueprint",
    "DataAsset",
    "PrimaryDataAsset",
    "DataTable",
    "CurveTable",
    "CurveFloat",
    "CurveVector",
    "CurveLinearColor",
    "UserDefinedStruct",
    "UserDefinedEnum",
    "BehaviorTree",
    "BlackboardData",
];
const RESOURCE_ONLY_CLASS_SUFFIXES: &[&str] = &[
    "Texture",
    "Texture2D",
    "TextureCube",
    "TextureRenderTarget2D",
    "TextureRenderTargetCube",
    "Material",
    "MaterialInstance",
    "MaterialFunction",
    "MaterialParameterCollection",
    "StaticMesh",
    "SkeletalMesh",
    "Skeleton",
    "PhysicsAsset",
    "AnimSequence",
    "AnimMontage",
    "BlendSpace",
    "BlendSpace1D",
    "AimOffsetBlendSpace",
    "AimOffsetBlendSpace1D",
    "PoseAsset",
    "SoundWave",
    "SoundCue",
    "SoundClass",
    "MetaSoundSource",
    "NiagaraSystem",
    "NiagaraEmitter",
    "ParticleSystem",
    "MediaSource",
    "MediaPlayer",
    "Font",
];

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
    let asset_path = to_asset_path(file_path);

    let parse_result = tokio::task::spawn_blocking(move || parse_asset_record(&path)).await;

    match parse_result {
        Ok(Ok(Some(record))) => {
            let mut graphs = state.asset_graphs.lock();

            if let Some(graph) = graphs.get_mut(&root_key) {
                remove_asset_from_graph(graph, &record.asset_path);
                insert_asset_record(graph, record);
                info!("Incremental asset update: {}", file_path.display());
            }
        }

        Ok(Ok(None)) => {
            let mut graphs = state.asset_graphs.lock();
            if let Some(graph) = graphs.get_mut(&root_key) {
                remove_asset_from_graph(graph, &asset_path);
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

pub fn remove_single_asset_from_state(state: &AppState, project_root: &str, file_path: &Path) {
    let root_key = normalize_path_key(project_root);
    let asset_path = to_asset_path(file_path);
    let mut graphs = state.asset_graphs.lock();
    if let Some(graph) = graphs.get_mut(&root_key) {
        remove_asset_from_graph(graph, &asset_path);
    }
}

pub fn populate_asset_graph_state(
    state: &AppState,
    project_root: &str,
    conn: &Connection,
) -> Result<()> {
    let graph = load_or_build_asset_graph(conn)?;
    let root_key = normalize_path_key(project_root);
    let mut graphs = state.asset_graphs.lock();
    graphs.insert(root_key, graph);
    Ok(())
}

pub fn ensure_asset_graph_state(
    state: &AppState,
    project_root: &str,
    conn: &Connection,
) -> bool {
    let root_key = normalize_path_key(project_root);
    {
        let graphs = state.asset_graphs.lock();
        if graphs.contains_key(&root_key) {
            return true;
        }
    }

    populate_asset_graph_state(state, project_root, conn).is_ok()
}

pub fn persist_asset_graph_state(
    state: &AppState,
    project_root: &str,
    primary_db_path: &str,
) -> Result<()> {
    let root_key = normalize_path_key(project_root);
    let graphs = state.asset_graphs.lock();
    let Some(graph) = graphs.get(&root_key) else {
        return Ok(());
    };
    runtime_index::save_asset_index(primary_db_path, &asset_runtime_from_graph(graph))
}

pub fn get_asset_usages_from_state(
    state: &AppState,
    project_root: &str,
    asset_path: &str,
) -> Option<Value> {
    let root_key = normalize_path_key(project_root);
    let graphs = state.asset_graphs.lock();
    let graph = graphs.get(&root_key)?;
    Some(asset_usages_from_graph(graph, asset_path))
}

pub fn get_asset_usage_hints_from_state(
    state: &AppState,
    project_root: &str,
    names: &[String],
) -> Option<Value> {
    let root_key = normalize_path_key(project_root);
    let graphs = state.asset_graphs.lock();
    let graph = graphs.get(&root_key)?;

    let mut seen_names = HashSet::new();
    let mut items = Vec::new();
    for raw_name in names {
        let name = raw_name.trim();
        if name.is_empty() {
            continue;
        }
        if !seen_names.insert(name.to_ascii_lowercase()) {
            continue;
        }

        items.push(json!({
            "name": name,
            "derived_count": asset_usage_count_from_graph(&graph.derived, name),
            "reference_count":
                asset_usage_count_from_graph(&graph.references, name)
                + asset_usage_count_from_graph(&graph.functions, name),
        }));
    }

    Some(Value::Array(items))
}

pub fn merge_derived_classes_from_state(
    state: &AppState,
    project_root: &str,
    base_class: &str,
    results: &mut Vec<Value>,
) -> bool {
    let root_key = normalize_path_key(project_root);
    let graphs = state.asset_graphs.lock();
    let Some(graph) = graphs.get(&root_key) else {
        return false;
    };

    let usage = asset_usages_from_graph(graph, base_class);
    let derived = usage
        .get("derived")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for item in derived {
        let Some(asset) = item.as_str() else {
            continue;
        };
        let exists = results.iter().any(|entry| {
            entry["path"]
                .as_str()
                .map(|path| path.eq_ignore_ascii_case(asset))
                .unwrap_or(false)
        });
        if !exists {
            results.push(json!({
                "name": asset.rsplit('/').next().unwrap_or(asset),
                "path": asset,
                "symbol_type": "uasset",
            }));
        }
    }

    true
}

/// Return true when the persistent asset index has been initialized at least once.
/// 判断持久化资产索引是否至少初始化过一次。
pub fn asset_index_initialized(conn: &Connection) -> Result<bool> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(false);
    };

    let initialized = read_asset_meta(&asset_conn, "asset_index_initialized")?;
    if !matches!(initialized.as_deref(), Some("1")) {
        return Ok(false);
    }

    let version = read_asset_meta(&asset_conn, "asset_index_version")?
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
    let overall_started_at = Instant::now();
    let initialized = asset_index_initialized(conn)?;
    let mut asset_conn = asset_db::open_asset_db_for_primary(conn)?
        .ok_or_else(|| anyhow::anyhow!("asset db path unavailable"))?;
    let content_dirs = discover_content_dirs(project_root);
    let asset_files = collect_candidate_assets(&content_dirs);
    info!(
        project_root = %project_root.display(),
        initialized = initialized,
        content_dirs = content_dirs.len(),
        candidate_assets = asset_files.len(),
        "Asset index refresh started"
    );

    if !initialized {
        let parse_started_at = Instant::now();
        let report = parse_asset_files(&asset_files, Some(reporter.as_ref()))?;
        let parse_elapsed_ms = parse_started_at.elapsed().as_millis();
        info!(
            project_root = %project_root.display(),
            elapsed_ms = parse_elapsed_ms,
            "Asset index full scan parsed: candidates={} indexed={} skipped={} errors={}",
            report.total_seen,
            report.parsed.len(),
            report.skipped,
            report.errors
        );
        reporter.report(
            "asset_index",
            0,
            report.parsed.len().max(1),
            "Persist",
        );
        let persist_started_at = Instant::now();
        replace_asset_index(&mut asset_conn, &report.parsed, Some(reporter.as_ref()))?;
        let persist_elapsed_ms = persist_started_at.elapsed().as_millis();
        reporter.report(
            "asset_index",
            report.parsed.len().max(1),
            report.parsed.len().max(1),
            "Ready",
        );
        info!(
            project_root = %project_root.display(),
            persist_elapsed_ms = persist_elapsed_ms,
            total_elapsed_ms = overall_started_at.elapsed().as_millis(),
            "Asset index full refresh finished"
        );
        let _ = persist_asset_runtime_index(conn);
        return Ok(());
    }

    let existing = load_existing_asset_mtimes(&asset_conn)?;
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

    let parse_started_at = Instant::now();
    let report = parse_asset_files(&changed_files, None)?;
    let parse_elapsed_ms = parse_started_at.elapsed().as_millis();
    let work_total = (deleted_paths.len() + report.parsed.len()).max(1);
    info!(
        project_root = %project_root.display(),
        elapsed_ms = parse_elapsed_ms,
        "Asset index delta parsed: candidates={} existing={} changed={} deleted={} indexed={} skipped={} errors={}",
        total_seen,
        existing.len(),
        changed_files.len(),
        deleted_paths.len(),
        report.parsed.len(),
        report.skipped,
        report.errors
    );

    reporter.report("asset_index", 0, work_total, "Persist");
    let persist_started_at = Instant::now();
    apply_asset_index_delta(
        &mut asset_conn,
        &report.parsed,
        &deleted_paths,
        Some(reporter.as_ref()),
    )?;
    reporter.report("asset_index", work_total, work_total, "Ready");
    info!(
        project_root = %project_root.display(),
        persist_elapsed_ms = persist_started_at.elapsed().as_millis(),
        total_elapsed_ms = overall_started_at.elapsed().as_millis(),
        "Asset index delta refresh finished"
    );
    let _ = persist_asset_runtime_index(conn);
    Ok(())
}

/// Persist a full asset index replacement.
/// 全量替换资产索引。
pub fn replace_asset_index(
    conn: &mut Connection,
    records: &[AssetRecord],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    with_asset_bulk_write(conn, |conn| {
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM asset_references", [])?;
        tx.execute("DELETE FROM asset_functions", [])?;
        tx.execute("DELETE FROM assets", [])?;
        tx.execute("DELETE FROM search_asset_usages", [])?;

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
            let mut stmt_usage = tx.prepare(
                "INSERT INTO search_asset_usages
                 (lookup_key, usage_kind, asset_path, asset_name, asset_name_lc, source_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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
                    &mut stmt_usage,
                    record,
                )?;
            }
        }

        write_asset_index_initialized(&tx)?;
        tx.commit()?;
        Ok(())
    })
}

fn apply_asset_index_delta(
    conn: &mut Connection,
    records: &[AssetRecord],
    deleted_paths: &[String],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    with_asset_bulk_write(conn, |conn| {
        let tx = conn.transaction()?;

        let total = (deleted_paths.len() + records.len()).max(1);
        let mut current = 0usize;
        let mut delete_keys = deleted_paths
            .iter()
            .map(|path| path.to_ascii_lowercase())
            .collect::<Vec<_>>();

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
            let mut stmt_usage = tx.prepare(
                "INSERT INTO search_asset_usages
                 (lookup_key, usage_kind, asset_path, asset_name, asset_name_lc, source_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for _source_path in deleted_paths {
                current += 1;
                if let Some(reporter) = reporter {
                    if current == total || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
                        reporter.report("asset_index", current, total, "Persist");
                    }
                }
            }

            for record in records {
                current += 1;
                if let Some(reporter) = reporter {
                    if current == total || current == 1 || current % ASSET_PROGRESS_EVERY == 0 {
                        reporter.report("asset_index", current, total, "Persist");
                    }
                }
                delete_keys.push(record.source_path.to_ascii_lowercase());
            }

            delete_assets_by_source_path_keys_tx(&tx, &delete_keys)?;

            for record in records {
                insert_asset_record_db(
                    &mut stmt_asset,
                    &mut stmt_reference,
                    &mut stmt_function,
                    &mut stmt_usage,
                    record,
                )?;
            }
        }

        write_asset_index_initialized(&tx)?;
        tx.commit()?;
        Ok(())
    })
}

/// Upsert one asset file into the persistent asset index.
/// 把单个资产文件写入持久化资产索引。
pub fn upsert_asset_file(conn: &mut Connection, file_path: &Path) -> Result<()> {
    let asset_conn = asset_db::open_asset_db_for_primary(conn)?
        .ok_or_else(|| anyhow::anyhow!("asset db path unavailable"))?;
    let record = parse_asset_record(file_path)?;
    let source_path = normalize_path(file_path);
    let tx = asset_conn.unchecked_transaction()?;

    delete_asset_by_source_path_tx(&tx, &source_path)?;

    if let Some(record) = record {
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
        let mut stmt_usage = tx.prepare(
            "INSERT INTO search_asset_usages
             (lookup_key, usage_kind, asset_path, asset_name, asset_name_lc, source_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        insert_asset_record_db(
            &mut stmt_asset,
            &mut stmt_reference,
            &mut stmt_function,
            &mut stmt_usage,
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
    let asset_conn = asset_db::open_asset_db_for_primary(conn)?
        .ok_or_else(|| anyhow::anyhow!("asset db path unavailable"))?;
    let tx = asset_conn.unchecked_transaction()?;
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
    pub asset_class: Option<String>,
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
    let started_at = Instant::now();
    let total_seen = asset_files.len();
    let completed = AtomicUsize::new(0);
    let reported_bucket = AtomicUsize::new(0);
    if let Some(reporter) = reporter {
        reporter.report("asset_index", 0, total_seen.max(1), "Scan");
    }

    let parsed_results = asset_files
        .par_iter()
        .filter_map(|path| {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string());

            let started_at = Instant::now();
            let result = match parse_asset_record(path) {
                Ok(record) => Some(Ok(record)),
                Err(err) => Some(Err((path.clone(), err))),
            };

            let elapsed_ms = started_at.elapsed().as_millis();
            if elapsed_ms >= 1000 {
                info!("Slow asset parse: {} took {} ms", filename, elapsed_ms);
            }

            let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if current > 0 && current % LOG_EVERY == 0 {
                debug!("Asset scan progress: {} files completed", current);
            }
            if let Some(reporter) = reporter {
                let bucket = (current * 1000 / total_seen.max(1)).min(1000);
                let previous = reported_bucket.load(Ordering::Relaxed);
                if current == total_seen
                    || current == 1
                    || current % ASSET_PROGRESS_EVERY == 0
                    || (bucket > previous
                        && bucket < 1000
                        && reported_bucket
                            .compare_exchange(previous, bucket, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok())
                {
                    reporter.report("asset_index", current, total_seen.max(1), "Scan");
                }
            }

            result
        })
        .collect::<Vec<_>>();

    let mut parsed = Vec::new();
    let mut skipped = 0usize;
    let mut errors = 0usize;
    let mut error_buckets = HashMap::<String, (usize, String)>::new();

    for item in parsed_results {
        match item {
            Ok(Some(record)) => parsed.push(record),
            Ok(None) => skipped += 1,
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
        skipped,
        errors,
        parsed,
        error_summary,
    })
    .map(|report| {
        info!(
            elapsed_ms = started_at.elapsed().as_millis(),
            total_seen = report.total_seen,
            indexed = report.parsed.len(),
            skipped = report.skipped,
            errors = report.errors,
            "Asset parse pass finished"
        );
        report
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

            if !is_index_candidate_asset_file(path) {
                continue;
            }

            let normalized = normalize_path(path);
            if seen.insert(normalized) {
                files.push(path.to_path_buf());
            }
        }
    }

    // Parse larger assets first so long-running work is distributed earlier
    // across rayon workers, reducing the 99% long tail.
    files.sort_unstable_by(|left, right| {
        let left_size = std::fs::metadata(left).map(|meta| meta.len()).unwrap_or(0);
        let right_size = std::fs::metadata(right).map(|meta| meta.len()).unwrap_or(0);
        right_size.cmp(&left_size).then_with(|| left.cmp(right))
    });

    files
}

/// Parse one asset file into an indexed AssetRecord when it participates in the logical asset index.
/// 把单个资产文件解析成逻辑资产索引记录；不需要完整索引时返回 None。
pub fn parse_asset_record(path: &Path) -> Result<Option<AssetRecord>> {
    let path = path.to_path_buf();
    let parse_path = path.clone();
    let sniff_started_at = Instant::now();

    match sniff_top_level_asset_class(&path) {
        Ok(Some(asset_class)) => {
            let sniff_elapsed_ms = sniff_started_at.elapsed().as_millis();
            if sniff_elapsed_ms >= 1000 {
                info!(
                    asset = %path.display(),
                    asset_class = %asset_class,
                    elapsed_ms = sniff_elapsed_ms,
                    "Slow asset class sniff"
                );
            }

            let class_name = asset_class_leaf(&asset_class);
            if is_resource_only_asset_class(class_name) {
                info!(
                    asset = %path.display(),
                    asset_class = %asset_class,
                    elapsed_ms = sniff_elapsed_ms,
                    "Skipping deep asset parse for resource-only asset class"
                );
                return Ok(None);
            }
        }
        Ok(None) => {
            let sniff_elapsed_ms = sniff_started_at.elapsed().as_millis();
            if sniff_elapsed_ms >= 1000 {
                info!(
                    asset = %path.display(),
                    elapsed_ms = sniff_elapsed_ms,
                    "Slow asset class sniff without resolved class"
                );
            }
        }
        Err(err) => {
            let sniff_elapsed_ms = sniff_started_at.elapsed().as_millis();
            if sniff_elapsed_ms >= 1000 {
                info!(
                    asset = %path.display(),
                    elapsed_ms = sniff_elapsed_ms,
                    error = %err,
                    "Slow asset class sniff before fallback"
                );
            }
        }
    }

    let parse_result = std::panic::catch_unwind(move || {
        let mut parser = UAssetParser::new();
        parser
            .parse(&parse_path)
            .map(|_| AssetRecord {
                asset_path: to_asset_path(&parse_path),
                source_path: normalize_path(&parse_path),
                mtime: file_mtime(&parse_path),
                asset_class: parser.asset_class,
                parent_class: parser.parent_class,
                imports: parser.imports,
                functions: parser.functions,
            })
    });

    match parse_result {
        Ok(Ok(record)) => Ok(filter_indexed_asset_record(path.as_path(), record)),
        Ok(Err(err)) => fallback_parse_asset_record(&path).or(Err(err)),
        Err(_) => fallback_parse_asset_record(&path)
            .or_else(|_| Err(anyhow::anyhow!("panic while parsing asset"))),
    }
}

fn fallback_parse_asset_record(path: &Path) -> Result<Option<AssetRecord>> {
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

    Ok(filter_indexed_asset_record(
        path,
        AssetRecord {
        asset_path,
        source_path: normalize_path(path),
        mtime: file_mtime(path),
        asset_class: None,
        parent_class,
        imports,
        functions,
        },
    ))
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

fn asset_runtime_from_graph(graph: &AssetGraph) -> AssetRuntimeIndex {
    AssetRuntimeIndex {
        references: runtime_map_from_graph(&graph.references),
        derived: runtime_map_from_graph(&graph.derived),
        functions: runtime_map_from_graph(&graph.functions),
    }
}

fn graph_from_asset_runtime(index: AssetRuntimeIndex) -> AssetGraph {
    AssetGraph {
        references: graph_map_from_runtime(index.references),
        derived: graph_map_from_runtime(index.derived),
        functions: graph_map_from_runtime(index.functions),
    }
}

fn runtime_map_from_graph(
    map: &HashMap<Arc<str>, HashSet<Arc<str>>>,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for (key, values) in map {
        let mut items = values.iter().map(|item| item.to_string()).collect::<Vec<_>>();
        items.sort_unstable();
        out.insert(key.to_string(), items);
    }
    out
}

fn graph_map_from_runtime(map: HashMap<String, Vec<String>>) -> HashMap<Arc<str>, HashSet<Arc<str>>> {
    let mut out = HashMap::new();
    for (key, values) in map {
        let mut items = HashSet::new();
        for value in values {
            items.insert(Arc::<str>::from(value));
        }
        out.insert(Arc::<str>::from(key), items);
    }
    out
}

fn load_asset_graph_from_db(conn: &Connection) -> Result<AssetGraph> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(AssetGraph::default());
    };

    let mut graph = AssetGraph::default();

    {
        let mut stmt = asset_conn.prepare("SELECT reference_key, asset_path FROM asset_references")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (lookup, asset_path) = row?;
            graph
                .references
                .entry(Arc::<str>::from(lookup))
                .or_default()
                .insert(Arc::<str>::from(asset_path));
        }
    }

    {
        let mut stmt = asset_conn.prepare("SELECT function_key, asset_path FROM asset_functions")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (lookup, asset_path) = row?;
            graph
                .functions
                .entry(Arc::<str>::from(lookup))
                .or_default()
                .insert(Arc::<str>::from(asset_path));
        }
    }

    {
        let mut stmt = asset_conn.prepare(
            "SELECT parent_class_key, asset_path
             FROM assets
             WHERE parent_class_key IS NOT NULL AND parent_class_key <> ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (lookup, asset_path) = row?;
            graph
                .derived
                .entry(Arc::<str>::from(lookup))
                .or_default()
                .insert(Arc::<str>::from(asset_path));
        }
    }

    Ok(graph)
}

fn load_or_build_asset_graph(conn: &Connection) -> Result<AssetGraph> {
    let primary_db_path = text::current_primary_db_path(conn)?;
    if let Some(primary_db_path) = primary_db_path.as_deref() {
        if let Some(index) = runtime_index::load_asset_index(primary_db_path)? {
            return Ok(graph_from_asset_runtime(index));
        }
    }

    let graph = load_asset_graph_from_db(conn)?;
    if let Some(primary_db_path) = primary_db_path.as_deref() {
        if let Err(err) = runtime_index::save_asset_index(primary_db_path, &asset_runtime_from_graph(&graph)) {
            warn!(
                "Failed to persist asset runtime index for {}: {}",
                primary_db_path,
                err
            );
        }
    }
    Ok(graph)
}

fn persist_asset_runtime_index(conn: &Connection) -> Result<()> {
    let Some(primary_db_path) = text::current_primary_db_path(conn)? else {
        return Ok(());
    };
    let graph = load_asset_graph_from_db(conn)?;
    runtime_index::save_asset_index(&primary_db_path, &asset_runtime_from_graph(&graph))
}

/// Query all indexed assets from the persistent DB.
/// 从持久化数据库获取全部已索引资产。
pub fn get_assets(conn: &Connection) -> Result<Value> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(json!([]));
    };
    let mut stmt = asset_conn.prepare("SELECT asset_path FROM assets ORDER BY asset_path COLLATE NOCASE")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let assets = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!(assets))
}

/// Query integrity and summary status for the logical asset index.
/// 查询逻辑资产索引的完整性和摘要状态。
pub fn get_asset_index_status(conn: &Connection) -> Result<Value> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(json!({
            "ok": false,
            "db_version": null,
            "asset_index_initialized": null,
            "asset_index_version": null,
            "expected_asset_index_version": ASSET_INDEX_VERSION,
            "counts": {
                "assets": 0,
                "asset_references": 0,
                "asset_functions": 0,
                "blueprint_like_assets": 0,
                "orphan_asset_references": 0,
                "orphan_asset_functions": 0,
            },
            "sample_assets": [],
            "issues": ["asset database is missing"],
        }));
    };
    let db_version = read_asset_meta(&asset_conn, "db_version")?;
    let asset_index_initialized = read_asset_meta(&asset_conn, "asset_index_initialized")?;
    let asset_index_version = read_asset_meta(&asset_conn, "asset_index_version")?;

    let assets_count = query_count(&asset_conn, "SELECT COUNT(*) FROM assets")?;
    let references_count = query_count(&asset_conn, "SELECT COUNT(*) FROM asset_references")?;
    let functions_count = query_count(&asset_conn, "SELECT COUNT(*) FROM asset_functions")?;
    let blueprint_like_count = query_count(
        &asset_conn,
        "SELECT COUNT(*) FROM assets
         WHERE parent_class IS NOT NULL
            OR asset_path LIKE '%.BP_%'
            OR asset_path LIKE '%/WBP_%'
            OR asset_path LIKE '%/WB_%'",
    )?;
    let orphan_references = query_count(
        &asset_conn,
        "SELECT COUNT(*)
         FROM asset_references ar
         LEFT JOIN assets a ON a.asset_path = ar.asset_path
         WHERE a.asset_path IS NULL",
    )?;
    let orphan_functions = query_count(
        &asset_conn,
        "SELECT COUNT(*)
         FROM asset_functions af
         LEFT JOIN assets a ON a.asset_path = af.asset_path
         WHERE a.asset_path IS NULL",
    )?;

    let sample_assets = query_single_column(
        &asset_conn,
        "SELECT asset_path FROM assets ORDER BY asset_path COLLATE NOCASE LIMIT 8",
    )?;

    let issues = build_asset_index_issues(
        &db_version,
        &asset_index_initialized,
        &asset_index_version,
        assets_count,
        references_count,
        orphan_references,
        orphan_functions,
    );

    Ok(json!({
        "ok": issues.is_empty(),
        "db_version": db_version,
        "asset_index_initialized": asset_index_initialized,
        "asset_index_version": asset_index_version,
        "expected_asset_index_version": ASSET_INDEX_VERSION,
        "counts": {
            "assets": assets_count,
            "asset_references": references_count,
            "asset_functions": functions_count,
            "blueprint_like_assets": blueprint_like_count,
            "orphan_asset_references": orphan_references,
            "orphan_asset_functions": orphan_functions,
        },
        "sample_assets": sample_assets,
        "issues": issues,
    }))
}

/// Query asset usages from the persistent DB.
/// 从持久化数据库查询资产引用和派生关系。
pub fn get_asset_usages(conn: &Connection, asset_path: &str) -> Result<Value> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(json!({
            "status": "ready",
            "references": [],
            "function_references": [],
            "derived": [],
        }));
    };
    let names = make_asset_lookup_names(asset_path);
    let mut references = HashSet::new();
    let mut function_references = HashSet::new();
    let mut derived = HashSet::new();

    let mut stmt_ref_or_derived = asset_conn.prepare(
        "SELECT DISTINCT asset_path
         FROM search_asset_usages
         WHERE usage_kind = ?1
           AND (lookup_key = ?2 OR lookup_key LIKE ?3)",
    )?;
    let mut stmt_func = asset_conn.prepare(
        "SELECT DISTINCT asset_path
         FROM search_asset_usages
         WHERE usage_kind = 'function'
           AND (lookup_key = ?1 OR lookup_key LIKE ?2 OR lookup_key LIKE ?3)",
    )?;

    for name in names {
        collect_asset_rows(
            &mut stmt_ref_or_derived,
            params!["reference", name, format!("%.{}", name)],
            &mut references,
        )?;
        collect_asset_rows(
            &mut stmt_func,
            params![name, format!("%.{}", name), format!("%:{}", name)],
            &mut function_references,
        )?;
        collect_asset_rows(
            &mut stmt_ref_or_derived,
            params!["derived", name, format!("%.{}", name)],
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

fn asset_usages_from_graph(graph: &AssetGraph, asset_path: &str) -> Value {
    let names = make_asset_lookup_names(asset_path);
    let mut references = HashSet::new();
    let mut function_references = HashSet::new();
    let mut derived = HashSet::new();

    for name in names {
        collect_asset_paths_from_map(&graph.references, &name, &mut references);
        collect_asset_paths_from_map(&graph.functions, &name, &mut function_references);
        collect_asset_paths_from_map(&graph.derived, &name, &mut derived);
    }

    json!({
        "status": "ready",
        "references": sorted_strings(references),
        "function_references": sorted_strings(function_references),
        "derived": sorted_strings(derived),
    })
}

fn collect_asset_paths_from_map(
    map: &HashMap<Arc<str>, HashSet<Arc<str>>>,
    lookup_name: &str,
    out: &mut HashSet<String>,
) {
    if let Some(items) = map.get(lookup_name) {
        for item in items {
            out.insert(item.to_string());
        }
    }
}

fn asset_usage_count_from_graph(
    map: &HashMap<Arc<str>, HashSet<Arc<str>>>,
    lookup_name: &str,
) -> usize {
    let mut seen = HashSet::<&str>::new();
    for name in make_asset_lookup_names(lookup_name) {
        if let Some(items) = map.get(name.as_str()) {
            for item in items {
                seen.insert(item.as_ref());
            }
        }
    }
    seen.len()
}

pub fn get_asset_usage_hints(conn: &Connection, names: &[String]) -> Result<Value> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(json!([]));
    };

    let mut stmt_ref_or_derived = asset_conn.prepare(
        "SELECT DISTINCT asset_path
         FROM search_asset_usages
         WHERE usage_kind = ?1
           AND (lookup_key = ?2 OR lookup_key LIKE ?3)",
    )?;
    let mut stmt_func = asset_conn.prepare(
        "SELECT DISTINCT asset_path
         FROM search_asset_usages
         WHERE usage_kind = 'function'
           AND (lookup_key = ?1 OR lookup_key LIKE ?2 OR lookup_key LIKE ?3)",
    )?;

    let mut seen_names = HashSet::new();
    let mut items = Vec::new();

    for name in names {
        let raw_name = name.trim();
        if raw_name.is_empty() {
            continue;
        }
        let normalized_name = raw_name.to_ascii_lowercase();
        if !seen_names.insert(normalized_name) {
            continue;
        }

        let mut references = HashSet::new();
        let mut function_references = HashSet::new();
        let mut derived = HashSet::new();

        for lookup_name in make_asset_lookup_names(raw_name) {
            collect_asset_rows(
                &mut stmt_ref_or_derived,
                params!["reference", lookup_name, format!("%.{}", lookup_name)],
                &mut references,
            )?;
            collect_asset_rows(
                &mut stmt_func,
                params![
                    lookup_name,
                    format!("%.{}", lookup_name),
                    format!("%:{}", lookup_name)
                ],
                &mut function_references,
            )?;
            collect_asset_rows(
                &mut stmt_ref_or_derived,
                params!["derived", lookup_name, format!("%.{}", lookup_name)],
                &mut derived,
            )?;
        }

        items.push(json!({
            "name": raw_name,
            "derived_count": derived.len(),
            "reference_count": references.len() + function_references.len(),
        }));
    }

    Ok(Value::Array(items))
}

/// Merge Blueprint-derived assets into a class query result from the persistent DB.
/// 从持久化数据库把蓝图派生资产合并到 class 查询结果。
pub fn merge_derived_classes(conn: &Connection, base_class: &str, results: &mut Vec<Value>) -> Result<()> {
    let Some(asset_conn) = asset_db::open_asset_db_read_only_for_primary(conn)? else {
        return Ok(());
    };
    let names = make_asset_lookup_names(base_class);
    let mut stmt = asset_conn.prepare(
        "SELECT DISTINCT asset_path
         FROM search_asset_usages
         WHERE usage_kind = 'derived'
           AND (lookup_key = ?1 OR lookup_key LIKE ?2)",
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
        "Intermediate"
            | "Binaries"
            | "Build"
            | "Saved"
            | ".git"
            | ".vs"
            | "DerivedDataCache"
            | "ExternalActors"
            | "ExternalObjects"
    )
}

/// Return true for candidate Unreal asset files that participate in the logical/reference index pipeline.
/// 判断是否是会参与逻辑/引用索引的 Unreal 候选资产文件。
fn is_index_candidate_asset_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if ext == "uasset"
    )
}

fn filter_indexed_asset_record(path: &Path, record: AssetRecord) -> Option<AssetRecord> {
    if should_fully_index_asset(path, &record) {
        Some(record)
    } else {
        None
    }
}

fn should_fully_index_asset(path: &Path, record: &AssetRecord) -> bool {
    if !matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("uasset")
    ) {
        return false;
    }

    if record.parent_class.is_some() {
        return true;
    }

    if let Some(asset_class) = record.asset_class.as_deref() {
        let class_name = asset_class_leaf(asset_class);

        if is_resource_only_asset_class(class_name) {
            return false;
        }

        if is_explicit_logical_asset_class(class_name) {
            return true;
        }

        if asset_class.starts_with("/Script/") || asset_class.starts_with("/Game/") {
            return true;
        }
    }

    imports_indicate_logical_asset(record) || likely_fallback_logical_asset(record)
}

fn imports_indicate_logical_asset(record: &AssetRecord) -> bool {
    record.imports.iter().any(|value| {
        let tail = asset_class_leaf(value);
        is_explicit_logical_asset_class(tail)
            || tail == "BlueprintGeneratedClass"
            || tail == "WidgetBlueprintGeneratedClass"
            || tail == "AnimBlueprintGeneratedClass"
    })
}

fn likely_fallback_logical_asset(record: &AssetRecord) -> bool {
    if record.functions.is_empty() {
        return false;
    }

    record
        .imports
        .iter()
        .any(|value| value.starts_with("/Script/") || value.starts_with("/Game/"))
}

fn asset_class_leaf(path: &str) -> &str {
    path.rsplit(['.', ':']).next().unwrap_or(path)
}

fn is_explicit_logical_asset_class(class_name: &str) -> bool {
    LOGICAL_ASSET_CLASS_SUFFIXES
        .iter()
        .any(|suffix| class_name.eq_ignore_ascii_case(suffix))
}

fn is_resource_only_asset_class(class_name: &str) -> bool {
    RESOURCE_ONLY_CLASS_SUFFIXES
        .iter()
        .any(|suffix| class_name.eq_ignore_ascii_case(suffix))
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
    stmt_usage: &mut rusqlite::Statement,
    record: &AssetRecord,
) -> Result<()> {
    let asset_key = record.asset_path.to_ascii_lowercase();
    let source_path_key = record.source_path.to_ascii_lowercase();
    let parent_class_key = record.parent_class.as_ref().map(|value| value.to_ascii_lowercase());
    let asset_name = record
        .asset_path
        .rsplit('/')
        .next()
        .unwrap_or(record.asset_path.as_str())
        .to_string();
    let asset_name_lc = asset_name.to_ascii_lowercase();

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
            stmt_usage.execute(params![
                key,
                "reference",
                record.asset_path.as_str(),
                asset_name.as_str(),
                asset_name_lc.as_str(),
                record.source_path.as_str(),
            ])?;
        }
    }

    let mut seen_functions = HashSet::new();
    for function in &record.functions {
        let key = function.to_ascii_lowercase();
        if seen_functions.insert(key.clone()) {
            stmt_function.execute(params![record.asset_path.as_str(), key])?;
            stmt_usage.execute(params![
                key,
                "function",
                record.asset_path.as_str(),
                asset_name.as_str(),
                asset_name_lc.as_str(),
                record.source_path.as_str(),
            ])?;
        }
    }

    if let Some(parent_key) = parent_class_key.as_deref() {
        stmt_usage.execute(params![
            parent_key,
            "derived",
            record.asset_path.as_str(),
            asset_name.as_str(),
            asset_name_lc.as_str(),
            record.source_path.as_str(),
        ])?;
    }

    Ok(())
}

fn delete_asset_by_source_path_tx(tx: &rusqlite::Transaction, source_path: &str) -> Result<()> {
    let source_path_key = source_path.to_ascii_lowercase();
    delete_assets_by_source_path_keys_tx(tx, &[source_path_key])
}

fn delete_assets_by_source_path_keys_tx(
    tx: &rusqlite::Transaction,
    source_path_keys: &[String],
) -> Result<()> {
    if source_path_keys.is_empty() {
        return Ok(());
    }

    tx.execute_batch(
        "DROP TABLE IF EXISTS temp_ucore_asset_source_keys;
         CREATE TEMP TABLE temp_ucore_asset_source_keys (source_path_key TEXT PRIMARY KEY);",
    )?;

    {
        let mut stmt =
            tx.prepare("INSERT OR IGNORE INTO temp_ucore_asset_source_keys (source_path_key) VALUES (?1)")?;
        for key in source_path_keys {
            stmt.execute([key.as_str()])?;
        }
    }

    tx.execute(
        "DELETE FROM search_asset_usages
         WHERE source_path IN (
             SELECT source_path
             FROM assets
             WHERE source_path_key IN (
                 SELECT source_path_key FROM temp_ucore_asset_source_keys
             )
         )",
        [],
    )?;

    tx.execute(
        "DELETE FROM assets
         WHERE source_path_key IN (
             SELECT source_path_key FROM temp_ucore_asset_source_keys
         )",
        [],
    )?;

    tx.execute_batch("DROP TABLE IF EXISTS temp_ucore_asset_source_keys;")?;

    Ok(())
}

fn with_asset_bulk_write<T, F>(conn: &mut Connection, work: F) -> Result<T>
where
    F: FnOnce(&mut Connection) -> Result<T>,
{
    prepare_asset_bulk_write(conn)?;
    let work_result = work(conn);
    let finalize_result = finalize_asset_bulk_write(conn);

    match (work_result, finalize_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err),
        (Err(work_err), Err(finalize_err)) => {
            warn!(
                "Asset bulk write failed and finalize also failed: work={}, finalize={}",
                work_err, finalize_err
            );
            Err(work_err)
        }
    }
}

fn prepare_asset_bulk_write(conn: &Connection) -> Result<()> {
    conn.busy_timeout(ASSET_BULK_BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", "-200000")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.execute("PRAGMA foreign_keys = OFF", [])?;
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_assets_asset_key;
         DROP INDEX IF EXISTS idx_assets_source_path_key;
         DROP INDEX IF EXISTS idx_assets_parent_class_key;
         DROP INDEX IF EXISTS idx_asset_references_asset_path;
         DROP INDEX IF EXISTS idx_asset_references_reference_key;
         DROP INDEX IF EXISTS idx_asset_functions_asset_path;
         DROP INDEX IF EXISTS idx_asset_functions_function_key;
         DROP INDEX IF EXISTS idx_search_asset_usages_lookup_kind;
         DROP INDEX IF EXISTS idx_search_asset_usages_asset_path;
         DROP INDEX IF EXISTS idx_search_asset_usages_asset_name_lc;",
    )?;
    Ok(())
}

fn finalize_asset_bulk_write(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_assets_asset_key ON assets(asset_key);
         CREATE INDEX IF NOT EXISTS idx_assets_source_path_key ON assets(source_path_key);
         CREATE INDEX IF NOT EXISTS idx_assets_parent_class_key ON assets(parent_class_key);
         CREATE INDEX IF NOT EXISTS idx_asset_references_asset_path ON asset_references(asset_path);
         CREATE INDEX IF NOT EXISTS idx_asset_references_reference_key ON asset_references(reference_key);
         CREATE INDEX IF NOT EXISTS idx_asset_functions_asset_path ON asset_functions(asset_path);
         CREATE INDEX IF NOT EXISTS idx_asset_functions_function_key ON asset_functions(function_key);
         CREATE INDEX IF NOT EXISTS idx_search_asset_usages_lookup_kind ON search_asset_usages(lookup_key, usage_kind);
         CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_path ON search_asset_usages(asset_path);
         CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_name_lc ON search_asset_usages(asset_name_lc);",
    )?;
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    conn.execute("PRAGMA optimize", [])?;
    Ok(())
}

fn write_asset_index_initialized(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO asset_meta (key, value)
         VALUES ('asset_index_initialized', '1')",
        [],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO asset_meta (key, value)
         VALUES ('asset_index_version', ?1)",
        [ASSET_INDEX_VERSION.to_string()],
    )?;
    Ok(())
}

fn read_asset_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM asset_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn query_count(conn: &Connection, sql: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap_or(0))
}

fn query_single_column(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row?);
    }
    Ok(values)
}

fn build_asset_index_issues(
    db_version: &Option<String>,
    asset_index_initialized: &Option<String>,
    asset_index_version: &Option<String>,
    assets_count: i64,
    references_count: i64,
    orphan_references: i64,
    orphan_functions: i64,
) -> Vec<String> {
    let mut issues = Vec::new();

    if db_version.is_none() {
        issues.push("project_meta.db_version is missing".to_string());
    }

    if !matches!(asset_index_initialized.as_deref(), Some("1")) {
        issues.push("asset_index_initialized is not set to 1".to_string());
    }

    let parsed_version = asset_index_version
        .as_deref()
        .and_then(|value| value.parse::<i32>().ok());
    if parsed_version != Some(ASSET_INDEX_VERSION) {
        issues.push(format!(
            "asset_index_version is {}, expected {}",
            asset_index_version.as_deref().unwrap_or("missing"),
            ASSET_INDEX_VERSION
        ));
    }

    if assets_count <= 0 {
        issues.push("assets table is empty".to_string());
    }

    if references_count <= 0 {
        issues.push("asset_references table is empty".to_string());
    }

    if orphan_references > 0 {
        issues.push(format!(
            "asset_references has {} orphan rows",
            orphan_references
        ));
    }

    if orphan_functions > 0 {
        issues.push(format!(
            "asset_functions has {} orphan rows",
            orphan_functions
        ));
    }

    issues
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
        asset_class_leaf, detect_parent_class_from_strings, extract_ascii_strings,
        is_explicit_logical_asset_class, is_resource_only_asset_class,
        looks_like_blueprint_function_token, looks_like_unreal_type_token, make_asset_lookup_names,
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

    #[test]
    fn logical_asset_class_list_covers_blueprints_and_data_assets() {
        assert!(is_explicit_logical_asset_class("Blueprint"));
        assert!(is_explicit_logical_asset_class("WidgetBlueprint"));
        assert!(is_explicit_logical_asset_class("PrimaryDataAsset"));
        assert!(is_explicit_logical_asset_class("BehaviorTree"));
    }

    #[test]
    fn resource_only_asset_class_list_covers_common_heavy_content() {
        assert!(is_resource_only_asset_class("Texture2D"));
        assert!(is_resource_only_asset_class("StaticMesh"));
        assert!(is_resource_only_asset_class("AnimMontage"));
        assert!(is_resource_only_asset_class("NiagaraSystem"));
    }

    #[test]
    fn asset_class_leaf_handles_script_and_function_paths() {
        assert_eq!(asset_class_leaf("/Script/Engine.Texture2D"), "Texture2D");
        assert_eq!(asset_class_leaf("/Game/Hero/BP_Hero.BP_Hero_C"), "BP_Hero_C");
        assert_eq!(
            asset_class_leaf("/Script/Game.SHero:RefreshCurrency"),
            "RefreshCurrency"
        );
    }
}
