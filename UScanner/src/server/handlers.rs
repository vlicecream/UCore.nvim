use anyhow::{anyhow, Result};
use notify::Watcher;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::db::text;
use crate::server::asset;
use crate::server::state::{AppState, ProjectContext, RpcProgressReporter};
use crate::server::utils::{
    convert_params, normalize_path_key, normalize_to_native, normalize_to_unix,
};
use crate::types::{
    ModifyResult, ModifyTargetAddModuleRequest, ModifyUprojectAddModuleRequest, QueryRequest,
    RefreshRequest, ScanRequest, SetupRequest, OpenBufferOverlay,
};
use crate::{db, query, refresh, scanner};

const SERVER_PROTOCOL_VERSION: u32 = 2;
const QUERY_PERF_WINDOW: Duration = Duration::from_secs(2);
const SLOW_QUERY_WARN_MS: u128 = 150;

static QUERY_PERF: OnceLock<Mutex<QueryPerfWindow>> = OnceLock::new();
static FAST_FIND_LOG_ENABLED: OnceLock<bool> = OnceLock::new();
static QUERY_LOG_ENABLED: OnceLock<bool> = OnceLock::new();

#[derive(Default)]
struct QueryPerfStat {
    count: u64,
    total_ms: u128,
    max_ms: u128,
    errors: u64,
}

struct QueryPerfWindow {
    started_at: Instant,
    stats: HashMap<&'static str, QueryPerfStat>,
}

#[derive(Clone)]
struct PartialQuerySender {
    tx: mpsc::Sender<Vec<u8>>,
    msgid: u64,
}

#[derive(Clone, Copy, Debug)]
struct SearchQueryProfile {
    is_identifier: bool,
    is_short: bool,
    is_sym_strong: bool,
    is_path_like: bool,
    is_literal_code: bool,
    is_text_like: bool,
    looks_like_unreal_type: bool,
}

impl SearchQueryProfile {
    fn classify(query: &str) -> Self {
        Self {
            is_identifier: live_find_is_identifier_query(query),
            is_short: live_find_is_short_query(query),
            is_sym_strong: live_find_is_sym_strong_query(query),
            is_path_like: live_find_is_path_like_query(query),
            is_literal_code: live_find_is_literal_code_query(query),
            is_text_like: live_find_is_text_like_query(query),
            looks_like_unreal_type: live_find_looks_like_unreal_type_query(query),
        }
    }

    fn tag_summary(&self) -> String {
        let mut tags = Vec::new();
        if self.is_short {
            tags.push("SHORT");
        }
        if self.is_sym_strong {
            tags.push("SYM_STRONG");
        }
        if self.is_path_like {
            tags.push("PATH_LIKE");
        }
        if self.is_literal_code {
            tags.push("LITERAL_CODE");
        }
        if self.is_text_like {
            tags.push("TEXT_LIKE");
        }
        if tags.is_empty() {
            return "GENERIC".to_string();
        }
        tags.join("|")
    }

    fn should_run_live_find_text_stage(&self, hqh: usize, repeated_query: bool) -> bool {
        if self.is_short || self.is_path_like {
            return false;
        }

        if hqh >= 10 {
            return false;
        }

        if self.is_sym_strong {
            return repeated_query && hqh == 0;
        }

        if self.is_literal_code || self.is_text_like {
            return true;
        }

        hqh < 4
    }

    fn should_force_reliable_text_search(&self, hqh: usize) -> bool {
        if self.is_short || self.is_path_like || self.is_sym_strong {
            return false;
        }

        hqh == 0 && (self.is_literal_code || self.is_text_like)
    }

    fn should_use_text_only_path(&self, query: &str) -> bool {
        if self.is_short || self.is_path_like || self.is_sym_strong {
            return false;
        }

        let query = query.trim();
        self.is_literal_code
            || query.contains(' ')
            || query.contains(';')
            || query.contains('"')
            || query.contains('#')
    }

    fn live_find_engine_skip_reason(
        &self,
        target: usize,
        project_result_count: usize,
        project_path_count: usize,
        project_hqh: usize,
        current_file_in_project: bool,
    ) -> Option<&'static str> {
        if project_result_count == 0 {
            return None;
        }

        if project_result_count >= target && project_hqh >= target.saturating_div(2).max(8) {
            return Some("project_saturated_hqh");
        }

        if self.is_sym_strong && project_result_count >= 24 {
            return Some("sym_strong_project_dense");
        }

        if self.is_identifier && !self.is_text_like && project_result_count >= 48 {
            return Some("identifier_project_dense");
        }

        if self.is_text_like && project_hqh >= 24 {
            return Some("text_like_project_hqh");
        }

        if current_file_in_project && project_result_count >= 24 && project_path_count >= 4 {
            return Some("current_project_dense");
        }

        if project_result_count >= 80 || project_path_count >= 12 {
            return Some("project_dense");
        }

        None
    }

    fn reference_engine_skip_reason(
        &self,
        scope: &str,
        project_result_count: usize,
        project_path_count: usize,
        found_definition: bool,
        cursor_in_project: bool,
    ) -> Option<&'static str> {
        if matches!(scope, "local" | "member") {
            return Some("scope_local");
        }

        if !cursor_in_project || project_result_count == 0 {
            return None;
        }

        if found_definition && project_result_count >= 24 {
            return Some("project_definition_dense");
        }

        if self.is_sym_strong {
            if self.looks_like_unreal_type && project_result_count >= 4 && project_path_count >= 2 {
                return Some("unreal_type_project_hits");
            }

            if project_result_count >= 8 {
                return Some("sym_strong_project_hits");
            }
        }

        if self.is_identifier && !self.is_text_like && project_result_count >= 16 {
            return Some("identifier_project_hits");
        }

        if project_result_count >= 40 || project_path_count >= 8 {
            return Some("project_hits_dense");
        }

        None
    }
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn hash_open_files(open_files: &[OpenBufferOverlay]) -> u64 {
    let mut entries = open_files
        .iter()
        .map(|item| (item.file_path.as_str(), item.content.as_str()))
        .collect::<Vec<_>>();

    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));

    let mut hasher = DefaultHasher::new();
    entries.len().hash(&mut hasher);

    for (file_path, content) in entries {
        file_path.hash(&mut hasher);
        content.hash(&mut hasher);
    }

    hasher.finish()
}

fn diagnostics_cache_key(
    file_path: Option<&str>,
    content: &str,
    open_files: &[OpenBufferOverlay],
) -> String {
    format!(
        "{}:{}:{}",
        file_path.unwrap_or("-"),
        hash_text(content),
        hash_open_files(open_files)
    )
}

fn navigation_cache_key(
    kind: &str,
    file_path: Option<&str>,
    content: &str,
    line: u32,
    character: u32,
    engine_db_path: Option<&str>,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        kind,
        file_path.unwrap_or("-"),
        hash_text(content),
        line,
        character,
        engine_db_path.unwrap_or("-")
    )
}

fn fast_find_log_enabled() -> bool {
    *FAST_FIND_LOG_ENABLED.get_or_init(|| {
        std::env::var("UCORE_FAST_FIND_LOG")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false)
    })
}

fn query_log_enabled() -> bool {
    *QUERY_LOG_ENABLED.get_or_init(|| {
        std::env::var("UCORE_QUERY_LOG")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false)
    })
}

fn sample_fast_find_results(results: &[Value]) -> String {
    results
        .iter()
        .take(6)
        .map(|item| {
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            let owner = item.get("class_name").and_then(Value::as_str).unwrap_or_default();
            let path = item
                .get("path")
                .or_else(|| item.get("file_path"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("{name}<{kind}> owner={owner} path={path}")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn sample_value_summary(value: &Value) -> String {
    if value.is_null() {
        return "null".to_string();
    }

    if let Some(items) = value.as_array() {
        let sample = items
            .first()
            .map(sample_single_item)
            .unwrap_or_else(|| "-".to_string());
        return format!("array count={} sample={}", items.len(), sample);
    }

    if let Some(items) = value.get("items").and_then(Value::as_array) {
        let sample = items
            .first()
            .map(sample_single_item)
            .unwrap_or_else(|| "-".to_string());
        return format!("items count={} sample={}", items.len(), sample);
    }

    if let Some(items) = value.get("results").and_then(Value::as_array) {
        let sample = items
            .first()
            .map(sample_single_item)
            .unwrap_or_else(|| "-".to_string());
        return format!("results count={} sample={}", items.len(), sample);
    }

    if let Some(items) = value.get("signatures").and_then(Value::as_array) {
        let sample = items
            .first()
            .map(sample_single_item)
            .unwrap_or_else(|| "-".to_string());
        return format!("signatures count={} sample={}", items.len(), sample);
    }

    if value.is_object() {
        return format!("object {}", sample_single_item(value));
    }

    value.to_string()
}

fn sample_single_item(value: &Value) -> String {
    let name = value
        .get("name")
        .or_else(|| value.get("label"))
        .or_else(|| value.get("symbol_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = value
        .get("type")
        .or_else(|| value.get("kind"))
        .or_else(|| value.get("symbol_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = value
        .get("path")
        .or_else(|| value.get("file_path"))
        .or_else(|| value.get("asset_path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = value
        .get("line")
        .or_else(|| value.get("line_number"))
        .and_then(Value::as_i64)
        .unwrap_or_default();

    format!("name={} kind={} path={} line={}", name, kind, path, line)
}

// -----------------------------------------------------------------------------
// Request types
// -----------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DeleteProjectRequest {
    pub project_root: String,
}

#[derive(Deserialize)]
pub struct PingRequest {
    pub pid: u32,
}

#[derive(Deserialize)]
pub struct ServerQueryRequest {
    pub project_root: String,

    #[serde(default)]
    pub engine_db_path: Option<String>,

    #[serde(flatten)]
    pub query: QueryRequest,
}

// -----------------------------------------------------------------------------
// Project lifecycle handlers
// -----------------------------------------------------------------------------

/// Delete a registered project from server state.
/// 从 server 状态里删除一个已注册工程。
pub async fn handle_delete_project(state: &AppState, params: &Value) -> Result<Value> {
    let req: DeleteProjectRequest = convert_params(params)?;
    let root_key = normalize_path_key(&req.project_root);

    let removed = {
        let mut projects = state.projects.lock();
        projects.remove(&root_key).is_some()
    };

    if !removed {
        return Err(anyhow!("Project not found: {}", root_key));
    }

    let _ = state.save_registry();
    info!("Deleted project: {}", root_key);

    Ok(Value::String("Deleted".to_string()))
}

/// Register a Neovim client heartbeat.
/// 注册 Neovim 客户端心跳。
pub async fn handle_ping(state: &AppState, params: &Value) -> Result<Value> {
    let req: PingRequest = convert_params(params)?;
    state.register_client(req.pid);
    Ok(Value::String("pong".to_string()))
}

/// Setup one project and open/create its database.
/// 初始化一个工程，并打开或创建对应数据库。
pub async fn handle_setup(state: Arc<AppState>, params: &Value) -> Result<Value> {
    let req: SetupRequest = convert_params(params)?;

    let root_key = normalize_path_key(&req.project_root);
    let db_path_unix = normalize_to_unix(&req.db_path);
    let db_path_native = normalize_to_native(&req.db_path);
    let cache_db_path_unix = req.cache_db_path.as_ref().map(|p| normalize_to_unix(p));

    drop_db_connections(&state, &db_path_native, cache_db_path_unix.as_deref());

    let readiness = ensure_database_ready(db_path_native.clone(), req.project_root.clone()).await?;
    info!(
        project_root = %req.project_root,
        db_path = %db_path_native,
        needs_full_refresh = readiness.needs_full_refresh,
        needs_asset_index = readiness.needs_asset_index,
        "setup readiness resolved"
    );

    {
        let mut projects = state.projects.lock();
        projects.insert(
            root_key.clone(),
            ProjectContext {
                db_path: db_path_unix,
                cache_db_path: cache_db_path_unix.clone(),
                vcs_hash: req.vcs_hash.clone(),
                last_refresh_at: Instant::now(),
            },
        );
    }

    let _ = state.get_connection(&db_path_native);

    if let Some(cache_path) = req.cache_db_path.as_ref() {
        let _ = state.get_persistent_cache_connection(&normalize_to_native(cache_path));
    }

    if !readiness.needs_full_refresh && readiness.needs_asset_index {
        info!(
            project_root = %req.project_root,
            "setup building asset index synchronously"
        );
        ensure_asset_index_ready(state.clone(), &db_path_native, &req.project_root).await?;
    }

    let _ = state.save_registry();

    Ok(json!({
        "status": "ok",
        "needs_full_refresh": readiness.needs_full_refresh,
    }))
}

/// Run a full project refresh.
/// 执行一次完整工程刷新。
pub async fn handle_refresh(
    state: &AppState,
    params: &Value,
    tx: mpsc::Sender<Vec<u8>>,
) -> Result<Value> {
    let mut req: RefreshRequest = convert_params(params)?;
    let root_key = normalize_path_key(&req.project_root);

    info!(
        project_root = %req.project_root,
        engine_root = ?req.engine_root,
        scope = ?req.scope,
        "refresh request received"
    );

    let _guard = RefreshGuard::try_new(state, root_key.clone())?;

    let db_path_unix = upsert_refresh_project_context(state, &mut req, &root_key)?;
    let db_path_native = normalize_to_native(&db_path_unix);

    let cache_path = {
        let projects = state.projects.lock();
        projects
            .get(&root_key)
            .and_then(|ctx| ctx.cache_db_path.clone())
    };

    drop_db_connections(state, &db_path_native, cache_path.as_deref());

    req.db_path = Some(db_path_unix.clone());
    let _ = state.save_registry();

    let reporter = Arc::new(RpcProgressReporter { tx });

    let refresh_project_root = req.project_root.clone();
    tokio::task::spawn_blocking(move || refresh::run_refresh(req, reporter)).await??;

    info!(
        project_root = %root_key,
        "refresh request finished"
    );

    clear_completion_cache(state, &root_key);

    if let Ok(conn) = state.get_connection(&db_path_native) {
        let conn = conn.lock();
        let _ = asset::populate_asset_graph_state(state, &refresh_project_root, &conn);
    }

    Ok(Value::String("Refresh success".to_string()))
}

/// Start filesystem watcher for a project root.
/// 启动工程目录文件监听。
pub async fn handle_watch(state: &AppState, params: &Value) -> Result<Value> {
    let req: crate::types::WatchRequest = convert_params(params)?;
    let root_native = normalize_to_native(&req.project_root);
    let root_path = PathBuf::from(&root_native);

    if !root_path.exists() {
        return Err(anyhow!("Path does not exist: {}", root_native));
    }

    let mut watcher = state.watcher.lock();

    watcher
        .watch(&root_path, notify::RecursiveMode::Recursive)
        .map_err(|err| {
            error!("Watcher failed for {}: {}", root_native, err);
            err
        })?;

    info!("Watcher started: {}", root_native);
    Ok(Value::String("Watch started".to_string()))
}

// -----------------------------------------------------------------------------
// Query handlers
// -----------------------------------------------------------------------------

/// Handle one query request from Neovim.
/// 处理来自 Neovim 的一次 query 请求。
pub async fn handle_query(
    state: Arc<AppState>,
    params: &Value,
    tx: mpsc::Sender<Vec<u8>>,
    msgid: u64,
) -> Result<Value> {
    let req: ServerQueryRequest = convert_params(params)?;
    let root_key = normalize_path_key(&req.project_root);
    let query_label = query_request_label(&req.query);
    let query_detail = query_request_detail(&req.query);
    let log_enabled = query_log_enabled();

    if log_enabled {
        info!(
            target: "ucore::query",
            "Query start: {}{}",
            query_label,
            query_detail
                .as_deref()
                .map(|detail| format!(" ({})", detail))
                .unwrap_or_default()
        );
    }

    if is_refreshing(&state, &root_key) {
        return Ok(json!([]));
    }

    let project = get_project_context(&state, &root_key)?;
    let db_path_native = normalize_to_native(&project.db_path);
    let cache_db_path_native = project.cache_db_path.as_ref().map(|p| normalize_to_native(p));

    let conn = state.get_read_only_connection(&db_path_native)?;
    let persistent_cache_conn = cache_db_path_native
        .as_deref()
        .and_then(|path| state.get_persistent_cache_connection(path).ok());

    let started_at = Instant::now();

    let result = tokio::task::spawn_blocking(move || {
        if let Some(value) = handle_state_query(
            state.clone(),
            &conn,
            &db_path_native,
            &root_key,
            &req.project_root,
            req.engine_db_path.clone(),
            req.query.clone(),
            Some(PartialQuerySender {
                tx: tx.clone(),
                msgid,
            }),
            persistent_cache_conn,
        )? {
            return Ok(value);
        }

        if is_streaming_query(&req.query) {
            query::process_query_streaming(&conn, req.query, move |items| {
                send_query_partial(&tx, msgid, items, true, false)?;
                Ok(())
            })
        } else {
            query::process_query(&conn, req.query)
        }
    })
    .await?;

    let elapsed = started_at.elapsed();
    record_query_perf(query_label, elapsed, result.is_err());

    if elapsed.as_millis() >= SLOW_QUERY_WARN_MS {
        match &result {
            Ok(_) => info!(
                "Slow query: {} took {} ms{}",
                query_label,
                elapsed.as_millis(),
                query_detail
                    .as_deref()
                    .map(|detail| format!(" ({})", detail))
                    .unwrap_or_default()
            ),
            Err(err) => warn!(
                "Slow query failed: {} took {} ms{}: {}",
                query_label,
                elapsed.as_millis(),
                query_detail
                    .as_deref()
                    .map(|detail| format!(" ({})", detail))
                    .unwrap_or_default(),
                err
            ),
        }
    }

    if log_enabled {
        match &result {
            Ok(value) => info!(
                target: "ucore::query",
                "Query done: {} took {} ms{} -> {}",
                query_label,
                elapsed.as_millis(),
                query_detail
                    .as_deref()
                    .map(|detail| format!(" ({})", detail))
                    .unwrap_or_default(),
                sample_value_summary(value)
            ),
            Err(err) => warn!(
                target: "ucore::query",
                "Query failed: {} took {} ms{} -> {}",
                query_label,
                elapsed.as_millis(),
                query_detail
                    .as_deref()
                    .map(|detail| format!(" ({})", detail))
                    .unwrap_or_default(),
                err
            ),
        }
    }

    result
}

/// Handle queries that need AppState instead of only SQLite.
/// 处理那些必须访问 AppState、不能只靠 SQLite 的 query。
fn handle_state_query(
    state: Arc<AppState>,
    conn: &rusqlite::Connection,
    project_db_path: &str,
    root_key: &str,
    project_root: &str,
    engine_db_path: Option<String>,
    request: QueryRequest,
    partial: Option<PartialQuerySender>,
    persistent_cache_conn: Option<Arc<parking_lot::Mutex<rusqlite::Connection>>>,
) -> Result<Option<Value>> {
    match request {
        QueryRequest::SearchSymbols {
            pattern,
            limit,
            offset,
        } => {
            let value =
                search_symbols_with_engine(state, conn, project_db_path, engine_db_path, &pattern, limit, offset)?;
            Ok(Some(value))
        }
        QueryRequest::SearchClassSymbols {
            pattern,
            limit,
            offset,
        } => {
            let value = search_class_symbols_with_engine(
                state,
                conn,
                project_db_path,
                engine_db_path,
                &pattern,
                limit,
                offset,
            )?;
            Ok(Some(value))
        }
        QueryRequest::FastFind {
            pattern,
            limit,
            offset,
            scope,
        } => {
            let value = fast_find_with_scope(
                state,
                conn,
                project_db_path,
                engine_db_path,
                &pattern,
                limit,
                offset,
                scope.as_deref(),
            )?;
            Ok(Some(value))
        }
        QueryRequest::SearchCodeText {
            pattern,
            limit,
            offset,
            scope,
        } => {
            let value = search_code_text_with_scope(
                state,
                conn,
                engine_db_path,
                &pattern,
                limit,
                offset,
                scope.as_deref(),
            )?;
            Ok(Some(value))
        }
        QueryRequest::UnifiedLiveFind {
            pattern,
            limit,
            offset,
            current_file,
            repeated_query,
        } => {
            let value = unified_live_find_with_engine(
                state,
                conn,
                project_db_path,
                engine_db_path,
                &pattern,
                limit,
                offset,
                current_file.as_deref(),
                repeated_query,
                partial.as_ref().filter(|_| offset == 0),
            )?;
            Ok(Some(value))
        }
        QueryRequest::GlobalFind {
            pattern,
            limit,
            offset,
        } => {
            let value =
                global_find_with_engine(state, conn, project_db_path, engine_db_path, &pattern, limit, offset)?;
            Ok(Some(value))
        }

        QueryRequest::FindSymbolUsages {
            symbol_name,
            file_path,
            content,
            line,
            character,
        } => {
            let value = find_references_with_engine(
                state,
                conn,
                project_db_path,
                engine_db_path,
                &symbol_name,
                file_path.as_deref(),
                content.as_deref(),
                line,
                character,
            )?;
            Ok(Some(value))
        }

        QueryRequest::GotoDefinition {
            content,
            line,
            character,
            file_path,
        } => {
            let value = goto_definition_with_engine(
                state,
                project_root,
                conn,
                project_db_path,
                engine_db_path,
                content,
                line,
                character,
                file_path,
            )?;

            Ok(Some(value))
        }

        QueryRequest::GotoImplementation {
            content,
            line,
            character,
            file_path,
        } => {
            let value = goto_implementation_with_engine(
                state,
                project_root,
                conn,
                project_db_path,
                engine_db_path,
                content,
                line,
                character,
                file_path,
            )?;

            Ok(Some(value))
        }

        QueryRequest::GetHover {
            content,
            line,
            character,
            file_path,
        } => {
            let value = hover_with_engine(
                state,
                project_root,
                conn,
                engine_db_path,
                content,
                line,
                character,
                file_path,
            )?;

            Ok(Some(value))
        }

        QueryRequest::GetSignatureHelp {
            content,
            line,
            character,
            file_path,
        } => {
            let value = signature_help_with_engine(
                state,
                conn,
                engine_db_path,
                content,
                line,
                character,
                file_path,
            )?;

            Ok(Some(value))
        }

        QueryRequest::GetAssetUsages { asset_path } => Ok(Some(
            {
                let _ = asset::ensure_asset_graph_state(&state, project_root, conn);
                asset::get_asset_usages_from_state(&state, project_root, &asset_path)
            }
                .unwrap_or(asset::get_asset_usages(conn, &asset_path)?),
        )),

        QueryRequest::GetAssetUsageHints { names } => Ok(Some(
            {
                let _ = asset::ensure_asset_graph_state(&state, project_root, conn);
                asset::get_asset_usage_hints_from_state(&state, project_root, &names)
            }
                .unwrap_or(asset::get_asset_usage_hints(conn, &names)?),
        )),

        QueryRequest::GetAssetIndexStatus => {
            Ok(Some(asset::get_asset_index_status(conn)?))
        }

        QueryRequest::GetAssetDependencies { asset_path } => {
            Ok(Some(get_asset_dependencies(project_root, &asset_path)?))
        }

        QueryRequest::FindDerivedClasses { base_class } => {
            let mut db_results = query::process_query(
                conn,
                QueryRequest::FindDerivedClasses {
                    base_class: base_class.clone(),
                },
            )?
            .as_array()
            .cloned()
            .unwrap_or_default();

            let _ = asset::ensure_asset_graph_state(&state, project_root, conn);
            if !asset::merge_derived_classes_from_state(&state, project_root, &base_class, &mut db_results) {
                asset::merge_derived_classes(conn, &base_class, &mut db_results)?;
            }

            Ok(Some(json!(db_results)))
        }

        QueryRequest::GetAssets => {
            Ok(Some(asset::get_assets(conn)?))
        }

        QueryRequest::GetConfigData { engine_root } => {
            let data =
                query::config::get_config_data_with_cache(&state, project_root, engine_root.as_deref())?;
            Ok(Some(json!(data)))
        }

        QueryRequest::GetCompletions {
            content,
            line,
            character,
            file_path,
        } => {
            let file_path_display = file_path
                .as_deref()
                .unwrap_or("-")
                .to_string();
            let cache = state.get_completion_cache(root_key);
            let engine_conn = match engine_db_path
                .as_deref()
                .map(normalize_to_native)
                .filter(|path| Path::new(path).is_file())
            {
                Some(path) => match state.get_read_only_connection(&path) {
                    Ok(conn) => Some(conn),
                    Err(err) => {
                        warn!("Failed to open Engine DB for completions: {}", err);
                        None
                    }
                },
                None => None,
            };

            let value = crate::completion::process_completion_with_engine(
                conn,
                engine_conn.as_ref(),
                &content,
                line,
                character,
                file_path,
                Some(cache),
                persistent_cache_conn,
            )?;

            debug!(
                "completion query handled: root={} file={} line={} char={} engine_db={}",
                root_key,
                file_path_display,
                line,
                character,
                engine_conn.is_some(),
            );

            Ok(Some(value))
        }

        QueryRequest::GetDiagnostics {
            content,
            file_path,
            open_files,
        } => {
            let cache = state.get_diagnostics_cache(root_key);
            let cache_key = diagnostics_cache_key(file_path.as_deref(), &content, &open_files);

            if let Some(value) = cache.lock().get(&cache_key) {
                debug!(
                    "diagnostics cache hit: root={} file={}",
                    root_key,
                    file_path.as_deref().unwrap_or("-"),
                );
                return Ok(Some(value));
            }

            if is_refreshing(&state, root_key) {
                info!(
                    "Diagnostics skipped during refresh: root={} file={}",
                    root_key,
                    file_path.as_deref().unwrap_or("-"),
                );
                return Ok(Some(json!({ "items": [] })));
            }

            let engine_conn = match engine_db_path
                .as_deref()
                .map(normalize_to_native)
                .filter(|path| Path::new(path).is_file())
            {
                Some(path) => match state.get_read_only_connection(&path) {
                    Ok(conn) => Some(conn),
                    Err(err) => {
                        warn!("Failed to open Engine DB for diagnostics: {}", err);
                        None
                    }
                },
                None => None,
            };

            let value = crate::diagnostics::process_diagnostics(
                conn,
                engine_conn.as_ref(),
                &content,
                file_path,
                &open_files,
            )?;
            cache.lock().put(cache_key, value.clone());
            Ok(Some(value))
        }

        QueryRequest::ParseBuildDiagnostics { output } => {
            Ok(Some(crate::diagnostics::parse_build_diagnostics(&output)))
        }

        _ => Ok(None),
    }
}

/// Return true for query variants that stream partial results.
/// 判断 query 是否需要流式分批返回。
fn is_streaming_query(request: &QueryRequest) -> bool {
    matches!(
        request,
        QueryRequest::GetFilesInModulesAsync { .. }
            | QueryRequest::SearchFilesInModulesAsync { .. }
            | QueryRequest::SearchFilesByPathPartAsync { .. }
            | QueryRequest::GetClassesInModulesAsync { .. }
            | QueryRequest::FindSymbolUsagesAsync { .. }
            | QueryRequest::GrepAssets { .. }
    )
}

/// Send one query partial notification through MessagePack RPC channel.
/// 通过 MessagePack RPC 通道发送一批 query partial 数据。
fn send_query_partial(
    tx: &mpsc::Sender<Vec<u8>>,
    msgid: u64,
    items: Vec<Value>,
    append: bool,
    done: bool,
) -> Result<()> {
    let notification = (2, "query/partial", json!({
        "msgid": msgid,
        "items": items,
        "append": append,
        "done": done,
    }));

    let payload = rmp_serde::to_vec(&notification)?;

    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);

    let _ = tx.blocking_send(framed);

    Ok(())
}

/// Go to definition in the project DB, then fall back to the shared Engine DB.
/// 先在项目 DB 里跳转定义，找不到时回退到共享 Engine DB。
fn goto_definition_with_engine(
    state: Arc<AppState>,
    project_root: &str,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    content: String,
    line: u32,
    character: u32,
    file_path: Option<String>,
) -> Result<Value> {
    let cache_key = navigation_cache_key(
        "goto_definition",
        file_path.as_deref(),
        &content,
        line,
        character,
        engine_db_path.as_deref(),
    );
    let navigation_cache = state.get_navigation_cache(project_root);
    if let Some(value) = navigation_cache.lock().get(&cache_key) {
        return Ok(value);
    }

    let project_nav_hot_index =
        load_navigation_hot_index(&state, project_db_path, "goto definition project");

    let mut project_result = query::goto::goto_definition_with_hot_index(
        project_conn,
        project_nav_hot_index.as_deref(),
        content.clone(),
        line,
        character,
        file_path.clone(),
    )?;

    if !project_result.is_null() {
        tag_value_source(&mut project_result, "project");
        navigation_cache.lock().put(cache_key, project_result.clone());
        return Ok(project_result);
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(Value::Null);
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(Value::Null);
    }

    let engine_conn = match state.get_shared_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for goto definition: {}", err);
            return Ok(Value::Null);
        }
    };
    let engine_nav_hot_index =
        load_navigation_hot_index(&state, &engine_db_path, "goto definition engine");
    let engine_content = content.clone();
    let engine_file_path = file_path.clone();

    let mut engine_result = match {
        let engine_conn = engine_conn.lock();
        query::goto::goto_definition_with_hot_index(
            &engine_conn,
            engine_nav_hot_index.as_deref(),
            engine_content,
            line,
            character,
            engine_file_path,
        )
    } {
        Ok(value) => value,
        Err(err) => {
            warn!("Failed to query Engine DB goto definition: {}", err);
            return Ok(Value::Null);
        }
    };

    if engine_result.is_null()
        && should_try_direct_engine_navigation_lookup(&content, line, character, false)
    {
        engine_result = match {
            let engine_conn = engine_conn.lock();
            query::goto::goto_definition(&engine_conn, content.clone(), line, character, file_path.clone())
        } {
            Ok(value) => value,
            Err(err) => {
                warn!("Failed to query Engine DB direct goto definition: {}", err);
                Value::Null
            }
        };
    }

    if !engine_result.is_null() {
        tag_value_source(&mut engine_result, "engine");
    }

    navigation_cache.lock().put(cache_key, engine_result.clone());

    Ok(engine_result)
}

fn should_try_direct_engine_navigation_lookup(
    content: &str,
    line: u32,
    character: u32,
    prefer_impl: bool,
) -> bool {
    let Some(ctx) = query::goto::extract_cursor_context(content, line, character) else {
        return false;
    };

    if matches!(ctx.qualifier_op.as_deref(), Some("::")) {
        return true;
    }

    if matches!(ctx.qualifier_op.as_deref(), Some(".") | Some("->")) {
        return false;
    }

    let symbol = ctx.symbol.trim();
    if symbol.is_empty() {
        return false;
    }

    if symbol
        .chars()
        .next()
        .map(|ch| ch.is_ascii_uppercase())
        .unwrap_or(false)
    {
        return true;
    }

    !prefer_impl && ctx.qualifier.is_none()
}

/// Go to implementation in the project DB, then fall back to the shared Engine DB.
/// 先在项目 DB 里跳转实现，找不到时回退到共享 Engine DB。
fn goto_implementation_with_engine(
    state: Arc<AppState>,
    project_root: &str,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    content: String,
    line: u32,
    character: u32,
    file_path: Option<String>,
) -> Result<Value> {
    let cache_key = navigation_cache_key(
        "goto_implementation",
        file_path.as_deref(),
        &content,
        line,
        character,
        engine_db_path.as_deref(),
    );
    let navigation_cache = state.get_navigation_cache(project_root);
    if let Some(value) = navigation_cache.lock().get(&cache_key) {
        return Ok(value);
    }

    let project_nav_hot_index =
        load_navigation_hot_index(&state, project_db_path, "goto implementation project");

    let mut project_result = query::goto::goto_implementation_with_hot_index(
        project_conn,
        project_nav_hot_index.as_deref(),
        content.clone(),
        line,
        character,
        file_path.clone(),
    )?;

    if !project_result.is_null() {
        tag_value_source(&mut project_result, "project");
        navigation_cache.lock().put(cache_key, project_result.clone());
        return Ok(project_result);
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(Value::Null);
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(Value::Null);
    }

    let engine_conn = match state.get_shared_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for goto implementation: {}", err);
            return Ok(Value::Null);
        }
    };
    let engine_nav_hot_index =
        load_navigation_hot_index(&state, &engine_db_path, "goto implementation engine");
    let engine_content = content.clone();
    let engine_file_path = file_path.clone();

    let mut engine_result = match {
        let engine_conn = engine_conn.lock();
        query::goto::goto_implementation_with_hot_index(
            &engine_conn,
            engine_nav_hot_index.as_deref(),
            engine_content,
            line,
            character,
            engine_file_path,
        )
    } {
        Ok(value) => value,
        Err(err) => {
            warn!("Failed to query Engine DB goto implementation: {}", err);
            return Ok(Value::Null);
        }
    };

    if engine_result.is_null()
        && should_try_direct_engine_navigation_lookup(&content, line, character, true)
    {
        engine_result = match {
            let engine_conn = engine_conn.lock();
            query::goto::goto_implementation(&engine_conn, content.clone(), line, character, file_path.clone())
        } {
            Ok(value) => value,
            Err(err) => {
                warn!("Failed to query Engine DB direct goto implementation: {}", err);
                Value::Null
            }
        };
    }

    if !engine_result.is_null() {
        tag_value_source(&mut engine_result, "engine");
    }

    navigation_cache.lock().put(cache_key, engine_result.clone());

    Ok(engine_result)
}

/// Resolve hover info in the project DB, then fall back to the shared Engine DB.
/// 先在项目 DB 里解析 hover，再回退到共享 Engine DB。
fn hover_with_engine(
    state: Arc<AppState>,
    project_root: &str,
    project_conn: &rusqlite::Connection,
    engine_db_path: Option<String>,
    content: String,
    line: u32,
    character: u32,
    file_path: Option<String>,
) -> Result<Value> {
    let cache_key = navigation_cache_key(
        "hover",
        file_path.as_deref(),
        &content,
        line,
        character,
        engine_db_path.as_deref(),
    );
    let navigation_cache = state.get_navigation_cache(project_root);
    if let Some(value) = navigation_cache.lock().get(&cache_key) {
        return Ok(value);
    }

    let mut project_result = query::goto::get_hover(
        project_conn,
        content.clone(),
        line,
        character,
        file_path.clone(),
    )?;

    if !project_result.is_null() {
        tag_value_source(&mut project_result, "project");
        navigation_cache.lock().put(cache_key, project_result.clone());
        return Ok(project_result);
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(Value::Null);
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(Value::Null);
    }

    let engine_conn = match state.get_shared_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for hover: {}", err);
            return Ok(Value::Null);
        }
    };

    let mut engine_result = match {
        let engine_conn = engine_conn.lock();
        query::goto::get_hover(&engine_conn, content, line, character, file_path)
    } {
        Ok(value) => value,
        Err(err) => {
            warn!("Failed to query Engine DB hover: {}", err);
            return Ok(Value::Null);
        }
    };

    if !engine_result.is_null() {
        tag_value_source(&mut engine_result, "engine");
    }

    navigation_cache.lock().put(cache_key, engine_result.clone());

    Ok(engine_result)
}

/// Resolve signature help in the project DB, then append Engine DB overloads.
/// 先在项目 DB 里解析签名帮助，再追加共享 Engine DB 的重载。
fn signature_help_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    engine_db_path: Option<String>,
    content: String,
    line: u32,
    character: u32,
    file_path: Option<String>,
) -> Result<Value> {
    let mut project_result = query::goto::get_signature_help(
        project_conn,
        content.clone(),
        line,
        character,
        file_path.clone(),
    )?;

    if project_result.is_null() {
        let Some(engine_db_path) = engine_db_path else {
            return Ok(Value::Null);
        };

        let engine_db_path = normalize_to_native(&engine_db_path);
        if !Path::new(&engine_db_path).is_file() {
            return Ok(Value::Null);
        }

        let engine_conn = match state.get_read_only_connection(&engine_db_path) {
            Ok(conn) => conn,
            Err(err) => {
                warn!("Failed to open Engine DB for signature help: {}", err);
                return Ok(Value::Null);
            }
        };

        let mut engine_result = match query::goto::get_signature_help(
            &engine_conn,
            content,
            line,
            character,
            file_path,
        ) {
            Ok(value) => value,
            Err(err) => {
                warn!("Failed to query Engine DB signature help: {}", err);
                return Ok(Value::Null);
            }
        };

        if !engine_result.is_null() {
            tag_value_source(&mut engine_result, "engine");
        }

        return Ok(engine_result);
    }

    if let Some(object) = project_result.as_object_mut() {
        if let Some(signatures) = object.get_mut("signatures").and_then(Value::as_array_mut) {
            tag_source(signatures, "project");
        }
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(project_result);
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(project_result);
    }

    let engine_conn = match state.get_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for signature help: {}", err);
            return Ok(project_result);
        }
    };

    let engine_value = match query::goto::get_signature_help(
        &engine_conn,
        content,
        line,
        character,
        file_path,
    ) {
        Ok(value) => value,
        Err(err) => {
            warn!("Failed to query Engine DB signature help: {}", err);
            return Ok(project_result);
        }
    };

    if engine_value.is_null() {
        return Ok(project_result);
    }

    let Some(project_object) = project_result.as_object_mut() else {
        return Ok(project_result);
    };
    let Some(project_signatures) = project_object
        .get_mut("signatures")
        .and_then(Value::as_array_mut)
    else {
        return Ok(project_result);
    };

    let mut engine_signatures = engine_value
        .get("signatures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tag_source(&mut engine_signatures, "engine");
    merge_query_results(project_signatures, engine_signatures, 16);

    Ok(project_result)
}

/// Find references in the project DB, then merge matching Engine DB results.
/// 先查询项目 DB 的引用，再合并共享 Engine DB 的引用结果。
fn find_references_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    symbol_name: &str,
    file_path: Option<&str>,
    content: Option<&str>,
    line: Option<u32>,
    character: Option<u32>,
) -> Result<Value> {
    let log_enabled = fast_find_log_enabled() || query_log_enabled();
    let profile = SearchQueryProfile::classify(symbol_name);
    let project_usage_hot_index =
        load_usage_hot_index(&state, project_db_path, "references project");
    let project_value = query::usage::find_symbol_usages_for_cursor_with_hot_index(
        project_conn,
        project_usage_hot_index.as_deref(),
        symbol_name,
        file_path,
        content,
        line,
        character,
    )?;
    let mut results = nested_results_array(&project_value);
    tag_source(&mut results, "project");

    let mut searched_files = project_value
        .get("searched_files")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut found_definition = project_value
        .get("found_definition")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let scope = project_value
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("global");
    let cursor_in_project = file_path
        .map(|path| file_exists_in_index(project_conn, path))
        .unwrap_or(false);
    let project_result_count = results.len();
    let project_path_count = result_path_count(&results);
    let skip_reason = profile.reference_engine_skip_reason(
        scope,
        project_result_count,
        project_path_count,
        found_definition,
        cursor_in_project,
    );

    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "FindReferences project symbol={:?} tags={} scope={} cursor_in_project={} project={} files={} found_definition={} engine_merge={} reason={}",
            symbol_name,
            profile.tag_summary(),
            scope,
            cursor_in_project,
            project_result_count,
            project_path_count,
            found_definition,
            skip_reason.is_none(),
            skip_reason.unwrap_or("eligible")
        );
    }

    if skip_reason.is_some() {
        return Ok(json!({
            "results": results,
            "searched_files": searched_files,
            "found_definition": found_definition,
            "scope": scope,
        }));
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(json!({
            "results": results,
            "searched_files": searched_files,
            "found_definition": found_definition,
            "scope": scope,
        }));
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(json!({
            "results": results,
            "searched_files": searched_files,
            "found_definition": found_definition,
            "scope": scope,
        }));
    }

    let engine_conn = match state.get_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for references: {}", err);
            return Ok(json!({
                "results": results,
                "searched_files": searched_files,
                "found_definition": found_definition,
                "scope": scope,
            }));
        }
    };

    let engine_usage_hot_index = load_usage_hot_index(&state, &engine_db_path, "references engine");
    match query::usage::find_symbol_usages_for_cursor_with_hot_index(
        &engine_conn,
        engine_usage_hot_index.as_deref(),
        symbol_name,
        file_path,
        content,
        line,
        character,
    ) {
        Ok(engine_value) => {
            searched_files += engine_value
                .get("searched_files")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            found_definition = found_definition
                || engine_value
                    .get("found_definition")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

            let mut engine_results = nested_results_array(&engine_value);
            tag_source(&mut engine_results, "engine");
            let engine_result_count = engine_results.len();
            merge_query_results(&mut results, engine_results, 300);

            if log_enabled {
                info!(
                    target: "ucore::fast_find",
                    "FindReferences merge symbol={:?} tags={} scope={} project={} engine={} total={} searched_files={} found_definition={}",
                    symbol_name,
                    profile.tag_summary(),
                    scope,
                    project_result_count,
                    engine_result_count,
                    results.len(),
                    searched_files,
                    found_definition
                );
            }
        }
        Err(err) => {
            warn!("Failed to query Engine DB references: {}", err);
        }
    }

    Ok(json!({
        "results": results,
        "searched_files": searched_files,
        "found_definition": found_definition,
        "scope": scope,
    }))
}

/// Add a source marker to one query result object.
/// 给单个查询结果对象添加来源标记。
fn tag_value_source(value: &mut Value, source: &str) {
    if let Some(object) = value.as_object_mut() {
        object.entry("source").or_insert_with(|| json!(source));
    }
}

/// Search symbols in the project DB, then merge matching Engine DB results.
/// 先查询项目 DB，再合并 Engine DB 的符号搜索结果。
fn search_symbols_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Value> {
    let limit = limit.clamp(1, 10_000);
    let project_hot_index = load_search_hot_index(&state, project_db_path, "symbol search project");
    let mut results = value_array(query::search::search_symbols_with_hot_index(
        project_conn,
        project_hot_index.as_deref(),
        pattern,
        limit,
        offset,
    )?);
    tag_source(&mut results, "project");

    if results.len() >= limit {
        results.truncate(limit);
        return Ok(json!(results));
    }

    let Some(engine_db_path) = engine_db_path else {
        return Ok(json!(results));
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(json!(results));
    }

    let engine_conn = match state.get_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for symbol search: {}", err);
            return Ok(json!(results));
        }
    };

    let remaining = limit.saturating_sub(results.len()).max(1);
    let engine_hot_index = load_search_hot_index(&state, &engine_db_path, "symbol search engine");
    let mut engine_results = match query::search::search_symbols_with_hot_index(
        &engine_conn,
        engine_hot_index.as_deref(),
        pattern,
        remaining,
        0,
    ) {
            Ok(value) => value_array(value),
            Err(err) => {
                warn!("Failed to query Engine DB symbols: {}", err);
                return Ok(json!(results));
            }
        };

    tag_source(&mut engine_results, "engine");
    merge_query_results(&mut results, engine_results, limit);

    Ok(json!(results))
}

fn global_find_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Value> {
    let limit = limit.clamp(1, 10_000);
    let project_hot_index = load_search_hot_index(&state, project_db_path, "global find project");
    let mut results = value_array(query::search::global_find_with_hot_index(
        project_conn,
        project_hot_index.as_deref(),
        pattern,
        limit,
        offset,
    )?);
    tag_source(&mut results, "project");

    if results.len() >= limit {
        return Ok(Value::Array(results));
    }

    let Some(engine_db_path) = engine_db_path.filter(|path| !path.trim().is_empty()) else {
        return Ok(Value::Array(results));
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(Value::Array(results));
    }

    let engine_conn = match state.get_read_only_connection(&engine_db_path) {
        Ok(conn) => conn,
        Err(err) => {
            warn!("Failed to open Engine DB for global find: {}", err);
            return Ok(Value::Array(results));
        }
    };
    let remaining = limit.saturating_sub(results.len()).max(1);
    let engine_hot_index = load_search_hot_index(&state, &engine_db_path, "global find engine");
    let mut engine_results = match query::search::global_find_with_hot_index(
        &engine_conn,
        engine_hot_index.as_deref(),
        pattern,
        remaining,
        0,
    ) {
        Ok(value) => value_array(value),
        Err(err) => {
            warn!("Failed to query Engine DB global find: {}", err);
            Vec::new()
        }
    };

    tag_source(&mut engine_results, "engine");
    results.extend(engine_results);
    results.truncate(limit);

    Ok(Value::Array(results))
}

fn fast_find_with_scope(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
    scope: Option<&str>,
) -> Result<Value> {
    let scope = scope.unwrap_or("both");
    let log_enabled = fast_find_log_enabled();
    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "FastFind request pattern={:?} scope={} limit={} offset={} engine_db={}",
            pattern,
            scope,
            limit,
            offset,
            engine_db_path.as_deref().unwrap_or("")
        );
    }
    if scope == "engine" {
        let engine_db_path_native = engine_db_path
            .as_deref()
            .map(normalize_to_native);
        let engine_hot_index = engine_db_path_native
            .as_deref()
            .and_then(|path| load_search_hot_index(&state, path, "fast find engine"));
        let Some(engine_conn) =
            open_engine_query_connection(state.clone(), engine_db_path, "fast find")?
        else {
            if log_enabled {
                info!(
                    target: "ucore::fast_find",
                    "FastFind engine pattern={:?} -> no engine db connection",
                    pattern
                );
            }
            return Ok(json!([]));
        };
        let mut results = {
            let engine_conn = engine_conn.lock();
            value_array(query::search::fast_find_with_hot_index(
                &engine_conn,
                engine_hot_index.as_deref(),
                pattern,
                limit,
                offset,
            )?)
        };
        tag_source(&mut results, "engine");
        if log_enabled {
            info!(
                target: "ucore::fast_find",
                "FastFind engine pattern={:?} count={} sample={}",
                pattern,
                results.len(),
                sample_fast_find_results(&results)
            );
        }
        return Ok(json!(results));
    }

    let project_hot_index = load_search_hot_index(&state, project_db_path, "fast find project");
    let mut results = value_array(query::search::fast_find_with_hot_index(
        project_conn,
        project_hot_index.as_deref(),
        pattern,
        limit,
        offset,
    )?);
    tag_source(&mut results, "project");
    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "FastFind project pattern={:?} count={} sample={}",
            pattern,
            results.len(),
            sample_fast_find_results(&results)
        );
    }

    if scope == "project" || results.len() >= limit {
        results.truncate(limit);
        if log_enabled {
            info!(
                target: "ucore::fast_find",
                "FastFind project-final pattern={:?} count={} sample={}",
                pattern,
                results.len(),
                sample_fast_find_results(&results)
            );
        }
        return Ok(json!(results));
    }

    let engine_db_path_native = engine_db_path.as_deref().map(normalize_to_native);
    let Some(engine_conn) =
        open_engine_query_connection(state.clone(), engine_db_path, "fast find")?
    else {
        if log_enabled {
            info!(
                target: "ucore::fast_find",
                "FastFind both pattern={:?} -> no engine db, returning project count={}",
                pattern,
                results.len()
            );
        }
        return Ok(json!(results));
    };
    let remaining = limit.saturating_sub(results.len()).max(1);
    let engine_hot_index = engine_db_path_native
        .as_deref()
        .and_then(|path| load_search_hot_index(&state, path, "fast find append"));
    let mut engine_results = {
        let engine_conn = engine_conn.lock();
        value_array(query::search::fast_find_with_hot_index(
            &engine_conn,
            engine_hot_index.as_deref(),
            pattern,
            remaining,
            0,
        )?)
    };
    tag_source(&mut engine_results, "engine");
    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "FastFind engine-append pattern={:?} count={} sample={}",
            pattern,
            engine_results.len(),
            sample_fast_find_results(&engine_results)
        );
    }
    merge_query_results(&mut results, engine_results, limit);
    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "FastFind merged pattern={:?} count={} sample={}",
            pattern,
            results.len(),
            sample_fast_find_results(&results)
        );
    }

    Ok(json!(results))
}

fn search_class_symbols_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    project_db_path: &str,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Value> {
    let limit = limit.clamp(1, 10_000);
    let project_hot_index =
        load_search_hot_index(&state, project_db_path, "class symbol search project");
    let mut results = value_array(query::search::search_class_symbols_with_hot_index(
        project_conn,
        project_hot_index.as_deref(),
        pattern,
        limit,
        0,
    )?);
    tag_source(&mut results, "project");

    if let Some(engine_db_path) = engine_db_path {
        let engine_db_path = normalize_to_native(&engine_db_path);
        if Path::new(&engine_db_path).is_file() {
            let engine_hot_index =
                load_search_hot_index(&state, &engine_db_path, "class symbol search engine");
            match state.get_read_only_connection(&engine_db_path) {
                Ok(engine_conn) => match query::search::search_class_symbols_with_hot_index(
                    &engine_conn,
                    engine_hot_index.as_deref(),
                    pattern,
                    limit,
                    0,
                ) {
                    Ok(value) => {
                        let mut engine_results = value_array(value);
                        tag_source(&mut engine_results, "engine");
                        results.extend(engine_results);
                    }
                    Err(err) => {
                        warn!("Failed to query Engine DB class symbols: {}", err);
                    }
                },
                Err(err) => {
                    warn!("Failed to open Engine DB for class symbol search: {}", err);
                }
            }
        }
    }

    dedupe_and_rank_class_symbol_results(&mut results, pattern);
    let page = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    Ok(json!(page))
}

fn search_code_text_with_scope(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
    scope: Option<&str>,
) -> Result<Value> {
    let scope = scope.unwrap_or("project");
    if scope == "engine" {
        let Some(engine_conn) = open_engine_query_connection(state, engine_db_path, "code text")? else {
            return Ok(json!([]));
        };
        let mut results = {
            let engine_conn = engine_conn.lock();
            value_array(query::search::search_code_text(&engine_conn, pattern, limit, offset)?)
        };
        tag_source(&mut results, "engine");
        return Ok(json!(results));
    }

    let mut results = value_array(query::search::search_code_text(project_conn, pattern, limit, offset)?);
    tag_source(&mut results, "project");
    Ok(json!(results))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MatchQuality {
    ExactSymbol = 0,
    PrefixSymbol = 1,
    NormalizedSymbol = 2,
    FileNameMatch = 3,
    OwnerClassMatch = 4,
    TextMatch = 5,
    Other = 6,
}

impl MatchQuality {
    fn label(self) -> &'static str {
        match self {
            MatchQuality::ExactSymbol => "exact_symbol",
            MatchQuality::PrefixSymbol => "prefix_symbol",
            MatchQuality::NormalizedSymbol => "normalized_symbol",
            MatchQuality::FileNameMatch => "file_name_match",
            MatchQuality::OwnerClassMatch => "owner_class_match",
            MatchQuality::TextMatch => "text_match",
            MatchQuality::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LiveFindSortKey {
    quality: MatchQuality,
    source_rank: u8,
    path_rank: u8,
    kind_rank: u8,
    name: String,
    path: String,
    line: i64,
    original_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct TextScanBudget {
    max_files: usize,
    max_bytes: usize,
    max_results: usize,
}

#[derive(Clone, Debug)]
struct TextScanPhaseReport {
    name: &'static str,
    files_scanned: usize,
    bytes_scanned: usize,
    results_found: usize,
    elapsed_ms: u128,
    stopped: bool,
}

struct LiveTextSearchState<'a> {
    original_pattern: &'a str,
    source: &'static str,
    total_result_limit: usize,
    results: Vec<Value>,
    seen_paths: HashSet<String>,
    seen_matches: HashSet<String>,
}

impl<'a> LiveTextSearchState<'a> {
    fn new(original_pattern: &'a str, source: &'static str, total_result_limit: usize) -> Self {
        Self {
            original_pattern,
            source,
            total_result_limit,
            results: Vec::new(),
            seen_paths: HashSet::new(),
            seen_matches: HashSet::new(),
        }
    }

    fn remaining_results(&self) -> usize {
        self.total_result_limit.saturating_sub(self.results.len())
    }

    fn is_done(&self) -> bool {
        self.results.len() >= self.total_result_limit
    }
}

fn unified_live_find_with_engine(
    state: Arc<AppState>,
    project_conn: &rusqlite::Connection,
    _project_db_path: &str,
    engine_db_path: Option<String>,
    pattern: &str,
    limit: usize,
    offset: usize,
    current_file: Option<&str>,
    repeated_query: bool,
    partial: Option<&PartialQuerySender>,
) -> Result<Value> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Ok(json!([]));
    }

    let limit = limit.clamp(1, 500);
    let offset = offset.min(1_000_000);
    let target = offset.saturating_add(limit).clamp(1, 500);
    let current_file = current_file
        .filter(|path| !path.trim().is_empty())
        .map(normalize_to_unix);
    let current_module = current_file
        .as_deref()
        .and_then(|path| module_name_for_file(project_conn, path));
    let log_enabled = fast_find_log_enabled() || query_log_enabled();
    let profile = SearchQueryProfile::classify(pattern);
    let current_file_in_project = current_file
        .as_deref()
        .map(|path| file_exists_in_index(project_conn, path))
        .unwrap_or(false);
    let send_partial = |items: &[Value], append: bool| -> Result<()> {
        if let Some(partial) = partial {
            send_query_partial(&partial.tx, partial.msgid, items.to_vec(), append, false)?;
        }
        Ok(())
    };

    if profile.should_use_text_only_path(pattern) {
        let mut results = value_array(query::search::search_code_text(project_conn, pattern, target, 0)?);
        tag_source(&mut results, "project");
        rank_live_find_results(
            &mut results,
            pattern,
            current_file.as_deref(),
            current_module.as_deref(),
        );

        let project_page = results
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        send_partial(&project_page, false)?;

        if repeated_query && results.len() < target {
            if let Some(engine_conn) =
                open_engine_query_connection(state.clone(), engine_db_path.clone(), "unified text-only live find")?
            {
                let mut engine_results = {
                    let engine_conn = engine_conn.lock();
                    value_array(query::search::search_code_text(&engine_conn, pattern, target, 0)?)
                };
                tag_source(&mut engine_results, "engine");
                merge_query_results(&mut results, engine_results, target);
                rank_live_find_results(
                    &mut results,
                    pattern,
                    current_file.as_deref(),
                    current_module.as_deref(),
                );
            }
        }

        let page = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        if partial.is_some() {
            return Ok(json!(live_find_append_delta(&page, &project_page)));
        }
        return Ok(json!(page));
    }

    let project_hot_index =
        load_search_hot_index(&state, _project_db_path, "unified live find project");

    let mut project_results = value_array(query::search::fast_find_with_hot_index(
        project_conn,
        project_hot_index.as_deref(),
        pattern,
        target,
        0,
    )?);
    tag_source(&mut project_results, "project");
    rank_live_find_results(
        &mut project_results,
        pattern,
        current_file.as_deref(),
        current_module.as_deref(),
    );

    let project_result_count = project_results.len();
    let project_path_count = result_path_count(&project_results);
    let project_hqh = live_find_hqh_count(&project_results);
    let skip_reason = profile.live_find_engine_skip_reason(
        target,
        project_result_count,
        project_path_count,
        project_hqh,
        current_file_in_project,
    );

    if log_enabled {
        info!(
            target: "ucore::fast_find",
            "UnifiedLiveFind project pattern={:?} tags={} repeated={} project={} hqh={} files={} current_file_in_project={} engine_merge={} reason={}",
            pattern,
            profile.tag_summary(),
            repeated_query,
            project_result_count,
            project_hqh,
            project_path_count,
            current_file_in_project,
            skip_reason.is_none(),
            skip_reason.unwrap_or("eligible")
        );
    }

    let mut results = project_results;
    let phase0_hqh = live_find_hqh_count(&results);
    let forced_reliable_text = profile.should_force_reliable_text_search(phase0_hqh);
    let should_run_text_stage = forced_reliable_text
        || should_run_live_find_text_stage(pattern, phase0_hqh, repeated_query);

    let mut text_count = 0usize;
    let mut phase_reports = Vec::new();
    if forced_reliable_text {
        let mut text_results =
            value_array(query::search::search_code_text(project_conn, pattern, target, 0)?);
        tag_source(&mut text_results, "project");
        text_count = text_results.len();
        if !text_results.is_empty() {
            results = text_results;
        }
    } else if should_run_text_stage {
        let text_limit = live_find_text_limit(pattern, limit, repeated_query);
        let mut text_state = LiveTextSearchState::new(pattern, "project", text_limit);
        run_project_live_text_search(
            project_conn,
            &current_file,
            current_module.as_deref(),
            pattern,
            repeated_query,
            &mut text_state,
            &mut phase_reports,
        )?;
        text_count = text_state.results.len();
        results.extend(text_state.results);
    }

    rank_live_find_results(
        &mut results,
        pattern,
        current_file.as_deref(),
        current_module.as_deref(),
    );

    let project_page = results
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    send_partial(&project_page, false)?;

    if skip_reason.is_none() && results.len() < target {
        let remaining = target.saturating_sub(results.len());
        let engine_target = remaining.max(limit.min(64)).clamp(1, target);
        let engine_db_path_native = engine_db_path.as_deref().map(normalize_to_native);
        let engine_hot_index = engine_db_path_native
            .as_deref()
            .and_then(|path| load_search_hot_index(&state, path, "unified live find engine"));
        if let Some(engine_conn) =
            open_engine_query_connection(state.clone(), engine_db_path.clone(), "unified live find")?
        {
            let mut engine_results = {
                let engine_conn = engine_conn.lock();
                value_array(query::search::fast_find_with_hot_index(
                    &engine_conn,
                    engine_hot_index.as_deref(),
                    pattern,
                    engine_target,
                    0,
                )?)
            };
            tag_source(&mut engine_results, "engine");
            merge_query_results(&mut results, engine_results, target);
            rank_live_find_results(
                &mut results,
                pattern,
                current_file.as_deref(),
                current_module.as_deref(),
            );
        }
    }

    if should_run_text_stage
        && repeated_query
        && results.len() < target
        && (live_find_is_text_like_query(pattern) || live_find_is_literal_code_query(pattern))
    {
        let text_limit = live_find_text_limit(pattern, limit, repeated_query);
        if text_count < text_limit {
            if let Some(engine_conn) =
                open_engine_query_connection(state.clone(), engine_db_path.clone(), "unified live text")?
            {
                let mut engine_text_state =
                    LiveTextSearchState::new(pattern, "engine", text_limit.saturating_sub(text_count));
                {
                    let engine_conn = engine_conn.lock();
                    run_engine_live_text_search(&engine_conn, pattern, &mut engine_text_state, &mut phase_reports)?;
                }
                text_count = text_count.saturating_add(engine_text_state.results.len());
                merge_query_results(&mut results, engine_text_state.results, target);
                rank_live_find_results(
                    &mut results,
                    pattern,
                    current_file.as_deref(),
                    current_module.as_deref(),
                );
            }
        }
    }

    if log_enabled && !phase_reports.is_empty() {
        let summary = phase_reports
            .iter()
            .map(|phase| {
                format!(
                    "{}:{}f/{}kb/{}r/{}ms{}",
                    phase.name,
                    phase.files_scanned,
                    phase.bytes_scanned / 1024,
                    phase.results_found,
                    phase.elapsed_ms,
                    if phase.stopped { ":stop" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        info!(
            target: "ucore::fast_find",
            "UnifiedLiveFind text phases pattern={:?} {}",
            pattern,
            summary
        );
    }

    if log_enabled {
        let phase0_count = results.len().saturating_sub(text_count);
        let tag_summary = live_find_tag_summary(pattern);
        info!(
            target: "ucore::fast_find",
            "UnifiedLiveFind pattern={:?} tags={} repeated={} phase0={} hqh={} text_stage={} text_count={} total={} sample={}",
            pattern,
            if forced_reliable_text {
                format!("{tag_summary}|FORCED_TEXT")
            } else {
                tag_summary
            },
            repeated_query,
            phase0_count,
            phase0_hqh,
            should_run_text_stage,
            text_count,
            results.len(),
            sample_fast_find_results(&results)
        );
    }

    let page = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    if partial.is_some() {
        return Ok(json!(live_find_append_delta(&page, &project_page)));
    }
    Ok(json!(page))
}

fn live_find_append_delta(final_page: &[Value], already_sent: &[Value]) -> Vec<Value> {
    let seen = already_sent
        .iter()
        .map(result_identity)
        .collect::<HashSet<_>>();
    final_page
        .iter()
        .filter(|item| !seen.contains(&result_identity(item)))
        .cloned()
        .collect()
}

fn live_find_tag_summary(pattern: &str) -> String {
    SearchQueryProfile::classify(pattern).tag_summary()
}

fn live_find_text_limit(pattern: &str, limit: usize, repeated_query: bool) -> usize {
    if repeated_query && (live_find_is_text_like_query(pattern) || live_find_is_literal_code_query(pattern)) {
        std::cmp::min(limit.max(40), 60)
    } else {
        std::cmp::min(limit.max(20), 30)
    }
}

fn should_run_live_find_text_stage(pattern: &str, hqh: usize, repeated_query: bool) -> bool {
    SearchQueryProfile::classify(pattern).should_run_live_find_text_stage(hqh, repeated_query)
}

fn live_find_is_identifier_query(query: &str) -> bool {
    let mut chars = query.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }

    chars.all(|ch| ch == '_' || ch == ':' || ch.is_ascii_alphanumeric())
}

fn live_find_is_short_query(query: &str) -> bool {
    query.trim().chars().count() < 4
}

fn live_find_is_sym_strong_query(query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() || !live_find_is_identifier_query(query) {
        return false;
    }

    if query.contains("::") {
        return true;
    }

    query
        .chars()
        .next()
        .map(|ch| ch.is_ascii_uppercase())
        .unwrap_or(false)
        || live_find_looks_like_unreal_type_query(query)
}

fn live_find_is_path_like_query(query: &str) -> bool {
    let lowered = query.trim().to_ascii_lowercase();
    lowered.contains('/') || lowered.contains('\\') || lowered.ends_with(".h") || lowered.ends_with(".cpp")
        || lowered.ends_with(".hpp") || lowered.ends_with(".inl")
}

fn live_find_is_literal_code_query(query: &str) -> bool {
    let query = query.trim();
    query.contains("->")
        || query.contains('.')
        || query.contains('(')
        || query.contains(')')
        || query.contains('=')
        || query.contains("::")
}

fn live_find_is_text_like_query(query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() || live_find_is_path_like_query(query) || live_find_is_sym_strong_query(query) {
        return false;
    }

    if live_find_is_identifier_query(query) && !query.contains('_') {
        return false;
    }

    query.contains(' ')
        || query.contains('_')
        || query
            .chars()
            .all(|ch| !ch.is_ascii_alphabetic() || ch.is_ascii_lowercase())
}

fn live_find_looks_like_unreal_type_query(query: &str) -> bool {
    let query = query.trim();
    if query.len() < 8 || query.contains('_') || query.contains("::") || !live_find_is_identifier_query(query) {
        return false;
    }

    matches!(
        query.chars().next().map(|ch| ch.to_ascii_lowercase()),
        Some('u' | 'a' | 'f' | 'e' | 'i')
    )
}

fn live_find_compact_identifier(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn live_find_source_rank(item: &Value) -> u8 {
    match item.get("source").and_then(Value::as_str).unwrap_or_default() {
        "project" => 0,
        "engine" => 1,
        _ => 2,
    }
}

fn live_find_kind_rank(item: &Value) -> u8 {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(
        kind.as_str(),
        "class" | "struct" | "enum" | "uclass" | "ustruct" | "uenum" | "uinterface"
    ) {
        0
    } else if kind == "define" || kind.contains("delegate") || kind.contains("event") {
        1
    } else if kind == "file" {
        2
    } else if kind.contains("function") || kind.contains("method") {
        3
    } else if kind == "text" {
        5
    } else {
        4
    }
}

fn live_find_path_rank(item: &Value, current_file: Option<&str>, current_module: Option<&str>) -> u8 {
    let path = item
        .get("path")
        .or_else(|| item.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path_key = normalize_path_key(path);
    let path_lower = path.to_ascii_lowercase();
    if current_file
        .map(normalize_path_key)
        .as_deref()
        .is_some_and(|current| current == path_key)
    {
        return 0;
    }

    if let Some(module) = current_module {
        if item
            .get("module_name")
            .and_then(Value::as_str)
            .is_some_and(|item_module| item_module.eq_ignore_ascii_case(module))
        {
            return 1;
        }
    }

    if path_lower.contains("/thirdparty/")
        || path_lower.contains("/source/thirdparty/")
        || path_lower.contains("/framework/libs/")
        || path_lower.contains("/external/")
    {
        return 4;
    }

    2
}

fn live_find_filename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase()
}

fn live_find_match_quality(item: &Value, pattern: &str) -> MatchQuality {
    let query = pattern.trim().to_ascii_lowercase();
    let compact_query = live_find_compact_identifier(pattern.trim());
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let owner = item
        .get("class_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = item
        .get("path")
        .or_else(|| item.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let filename = live_find_filename(&path);

    if kind == "text" {
        return MatchQuality::TextMatch;
    }

    if is_class_like_kind(&kind) && matches_unreal_type_fragment(&name, &query) {
        return MatchQuality::ExactSymbol;
    }

    if name == query && kind != "file" {
        return MatchQuality::ExactSymbol;
    }
    if !query.is_empty() && name.starts_with(&query) && kind != "file" {
        return MatchQuality::PrefixSymbol;
    }

    if !compact_query.is_empty() && kind != "file" {
        let compact_name = live_find_compact_identifier(&name);
        if !compact_name.is_empty()
            && (compact_name == compact_query
                || compact_name.starts_with(&compact_query)
                || compact_name.contains(&compact_query)
                || live_find_is_compact_fuzzy_match(&compact_name, &compact_query))
        {
            return MatchQuality::NormalizedSymbol;
        }
    } else if !query.is_empty() && name.contains(&query) && kind != "file" {
        return MatchQuality::NormalizedSymbol;
    }

    if kind == "file"
        || filename == query
        || (!query.is_empty() && (filename.starts_with(&query) || filename.contains(&query)))
        || (!compact_query.is_empty()
            && live_find_is_compact_fuzzy_match(&live_find_compact_identifier(&filename), &compact_query))
    {
        return MatchQuality::FileNameMatch;
    }

    if !query.is_empty() && owner.contains(&query) {
        return MatchQuality::OwnerClassMatch;
    }

    if !query.is_empty() && path.contains(&query) {
        return MatchQuality::FileNameMatch;
    }

    MatchQuality::Other
}

fn live_find_is_compact_fuzzy_match(compact_name: &str, compact_query: &str) -> bool {
    if compact_name.is_empty() || compact_query.is_empty() || compact_query.len() < 3 {
        return false;
    }

    let name_chars = compact_name.chars().collect::<Vec<_>>();
    let query_chars = compact_query.chars().collect::<Vec<_>>();
    let mut query_index = 0usize;
    let mut first_match = None;
    let mut last_match = 0usize;
    let mut previous_match = None;
    let mut max_gap = 0usize;

    for (index, ch) in name_chars.iter().enumerate() {
        if query_index >= query_chars.len() {
            break;
        }

        if *ch != query_chars[query_index] {
            continue;
        }

        first_match.get_or_insert(index);
        if let Some(previous) = previous_match {
            max_gap = max_gap.max(index.saturating_sub(previous + 1));
        }
        previous_match = Some(index);
        last_match = index;
        query_index += 1;
    }

    if query_index != query_chars.len() {
        return false;
    }

    let span = last_match.saturating_sub(first_match.unwrap_or(0)) + 1;
    let extra = span.saturating_sub(query_chars.len());
    let max_extra = if compact_query.len() <= 4 { 1 } else { 2 };
    let max_gap_allowed = if compact_query.len() <= 4 { 1 } else { 2 };

    extra <= max_extra && max_gap <= max_gap_allowed
}

fn is_class_like_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "struct" | "enum" | "uclass" | "ustruct" | "uenum" | "uinterface"
    )
}

fn matches_unreal_type_fragment(name: &str, query: &str) -> bool {
    if query.is_empty() || query.len() < 6 {
        return false;
    }

    let compact_name = live_find_compact_identifier(name);
    if compact_name.is_empty() {
        return false;
    }

    let compact_query = live_find_compact_identifier(query);
    if compact_query.is_empty() {
        return false;
    }

    if compact_name == compact_query {
        return true;
    }

    ["u", "a", "f", "e", "i"]
        .iter()
        .any(|prefix| compact_name == format!("{prefix}{compact_query}"))
}

fn live_find_sort_key(
    item: &Value,
    pattern: &str,
    current_file: Option<&str>,
    current_module: Option<&str>,
    original_index: usize,
) -> LiveFindSortKey {
    let quality = live_find_match_quality(item, pattern);
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = item
        .get("path")
        .or_else(|| item.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let line = item.get("line").and_then(Value::as_i64).unwrap_or(1);

    LiveFindSortKey {
        quality,
        source_rank: live_find_source_rank(item),
        path_rank: live_find_path_rank(item, current_file, current_module),
        kind_rank: live_find_kind_rank(item),
        name,
        path,
        line,
        original_index,
    }
}

fn live_find_identity(item: &Value) -> String {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = item
        .get("path")
        .or_else(|| item.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = item
        .get("line")
        .or_else(|| item.get("line_number"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let kind = item
        .get("type")
        .or_else(|| item.get("symbol_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{kind}\t{name}\t{path}\t{line}")
}

fn rank_live_find_results(
    results: &mut Vec<Value>,
    pattern: &str,
    current_file: Option<&str>,
    current_module: Option<&str>,
) {
    let mut ranked = results
        .drain(..)
        .enumerate()
        .map(|(index, item)| {
            let key = live_find_sort_key(&item, pattern, current_file, current_module, index);
            (key, item)
        })
        .filter(|(key, _)| live_find_should_keep_match(pattern, key.quality))
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| left.0.cmp(&right.0));

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (backend_rank, (key, mut item)) in ranked.into_iter().enumerate() {
        if !seen.insert(live_find_identity(&item)) {
            continue;
        }

        if let Some(object) = item.as_object_mut() {
            object.insert("match_quality".to_string(), json!(key.quality.label()));
            object.insert("backend_rank".to_string(), json!(backend_rank as i64));
        }
        out.push(item);
    }

    *results = out;
}

fn live_find_should_keep_match(pattern: &str, quality: MatchQuality) -> bool {
    let _ = pattern;
    quality != MatchQuality::Other
}

fn live_find_hqh_count(results: &[Value]) -> usize {
    results
        .iter()
        .filter(|item| {
            let quality = item
                .get("match_quality")
                .and_then(Value::as_str)
                .unwrap_or_default();
            matches!(quality, "exact_symbol" | "prefix_symbol" | "normalized_symbol")
        })
        .count()
}

fn file_exists_in_index(conn: &rusqlite::Connection, path: &str) -> bool {
    let path = normalize_to_unix(path);
    conn.query_row(
        r#"
        SELECT 1
        FROM files f
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        WHERE lower(dp.full_path || '/' || sn.text) = lower(?)
        LIMIT 1
        "#,
        [path.as_str()],
        |_| Ok(()),
    )
    .is_ok()
}

fn result_path_count(results: &[Value]) -> usize {
    results
        .iter()
        .filter_map(|item| {
            item.get("path")
                .or_else(|| item.get("file_path"))
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
        })
        .collect::<HashSet<_>>()
        .len()
}

fn module_name_for_file(conn: &rusqlite::Connection, path: &str) -> Option<String> {
    conn.query_row(
        r#"
        SELECT sm.text
        FROM files f
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        LEFT JOIN modules m ON f.module_id = m.id
        LEFT JOIN strings sm ON m.name_id = sm.id
        WHERE lower(dp.full_path || '/' || sn.text) = lower(?)
        LIMIT 1
        "#,
        [path],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

fn run_project_live_text_search(
    conn: &rusqlite::Connection,
    current_file: &Option<String>,
    current_module: Option<&str>,
    pattern: &str,
    repeated_query: bool,
    state: &mut LiveTextSearchState<'_>,
    reports: &mut Vec<TextScanPhaseReport>,
) -> Result<()> {
    let phase1_budget = TextScanBudget {
        max_files: 50,
        max_bytes: 8 * 1024 * 1024,
        max_results: std::cmp::min(state.remaining_results(), 10),
    };

    if let Some(path) = current_file.as_deref() {
        let report = scan_single_text_path(conn, "project_file", path, state, phase1_budget)?;
        reports.push(report);
    }

    if !state.is_done() {
        if let Some(module) = current_module.filter(|value| !value.trim().is_empty()) {
            let report = scan_text_index_matches(conn, state, phase1_budget, "project_module", |path| {
                module_name_for_file(conn, path)
                    .as_deref()
                    .is_some_and(|item_module| item_module.eq_ignore_ascii_case(module))
            })?;
            reports.push(report);
        }
    }

    if state.is_done() {
        return Ok(());
    }

    let should_expand_project =
        live_find_is_text_like_query(pattern) || live_find_is_literal_code_query(pattern) || repeated_query;
    if !should_expand_project {
        return Ok(());
    }

    let plugin_root = current_file
        .as_deref()
        .and_then(live_find_plugin_root)
        .map(|value| value.to_ascii_lowercase());
    let phase2_budget = TextScanBudget {
        max_files: 200,
        max_bytes: 32 * 1024 * 1024,
        max_results: std::cmp::min(state.remaining_results(), 30),
    };

    if let Some(plugin_root) = plugin_root.as_deref() {
        if !state.is_done() {
            let report = scan_text_index_matches(conn, state, phase2_budget, "project_plugin", |path| {
                path.to_ascii_lowercase().starts_with(plugin_root)
            })?;
            reports.push(report);
        }
    }

    if !state.is_done() {
        let report = scan_text_index_matches(conn, state, phase2_budget, "project_all", |_| true)?;
        reports.push(report);
    }

    Ok(())
}

fn run_engine_live_text_search(
    conn: &rusqlite::Connection,
    _pattern: &str,
    state: &mut LiveTextSearchState<'_>,
    reports: &mut Vec<TextScanPhaseReport>,
) -> Result<()> {
    if state.is_done() {
        return Ok(());
    }

    let phase3_budget = TextScanBudget {
        max_files: 1000,
        max_bytes: 64 * 1024 * 1024,
        max_results: std::cmp::min(state.remaining_results(), 50),
    };
    let report = scan_text_index_matches(conn, state, phase3_budget, "engine_all", |_| true)?;
    reports.push(report);
    Ok(())
}

fn scan_single_text_path(
    conn: &rusqlite::Connection,
    phase_name: &'static str,
    path: &str,
    state: &mut LiveTextSearchState<'_>,
    budget: TextScanBudget,
) -> Result<TextScanPhaseReport> {
    let started = Instant::now();
    let mut files_scanned = 0usize;
    let mut bytes_scanned = 0usize;
    let before = state.results.len();
    let mut stopped = false;

    if is_live_text_searchable_path(path) && !state.is_done() && budget.max_results > 0 {
        let matches = text::search_matching_lines_in_paths(
            conn,
            state.original_pattern,
            &[path.to_string()],
            budget.max_results.max(8),
            0,
        )?;
        if !matches.is_empty() {
            files_scanned = 1;
        }
        for item in matches {
            bytes_scanned = bytes_scanned.saturating_add(item.line_text.len());
            if !push_live_text_match(state, item) {
                continue;
            }
            if bytes_scanned >= budget.max_bytes || state.is_done() || state.results.len() >= budget.max_results {
                stopped = true;
                break;
            }
        }
    }

    Ok(TextScanPhaseReport {
        name: phase_name,
        files_scanned,
        bytes_scanned,
        results_found: state.results.len().saturating_sub(before),
        elapsed_ms: started.elapsed().as_millis(),
        stopped,
    })
}

fn scan_text_index_matches<P>(
    conn: &rusqlite::Connection,
    state: &mut LiveTextSearchState<'_>,
    budget: TextScanBudget,
    phase_name: &'static str,
    mut allow_path: P,
) -> Result<TextScanPhaseReport>
where
    P: FnMut(&str) -> bool,
{
    let started = Instant::now();
    let mut files_scanned = 0usize;
    let mut bytes_scanned = 0usize;
    let before = state.results.len();
    let mut stopped = false;

    if budget.max_results == 0 || state.is_done() {
        return Ok(TextScanPhaseReport {
            name: phase_name,
            files_scanned,
            bytes_scanned,
            results_found: 0,
            elapsed_ms: started.elapsed().as_millis(),
            stopped: true,
        });
    }

    let candidate_limit = std::cmp::max(
        256,
        std::cmp::max(
            budget.max_files.saturating_mul(24),
            budget.max_results.saturating_mul(24),
        ),
    )
    .min(8192);
    let candidate_lines = text::search_matching_lines(conn, state.original_pattern, candidate_limit, 0)?;
    let mut seen_phase_paths = HashSet::new();

    for item in candidate_lines {
        let path = normalize_to_unix(&item.path);
        if !allow_path(&path) || !is_live_text_searchable_path(&path) {
            continue;
        }
        if seen_phase_paths.insert(path.clone()) {
            files_scanned += 1;
        }
        if files_scanned > budget.max_files || bytes_scanned >= budget.max_bytes || state.is_done() {
            stopped = true;
            break;
        }

        bytes_scanned = bytes_scanned.saturating_add(item.line_text.len());
        let _ = push_live_text_match(state, item);
        if bytes_scanned >= budget.max_bytes || state.is_done() || state.results.len() >= budget.max_results {
            stopped = true;
            break;
        }
    }

    Ok(TextScanPhaseReport {
        name: phase_name,
        files_scanned,
        bytes_scanned,
        results_found: state.results.len().saturating_sub(before),
        elapsed_ms: started.elapsed().as_millis(),
        stopped,
    })
}

fn push_live_text_match(state: &mut LiveTextSearchState<'_>, item: text::TextLineMatch) -> bool {
    let normalized_path = normalize_to_unix(&item.path);
    let match_key = format!("{}\t{}", normalized_path, item.line_number);
    if !state.seen_matches.insert(match_key) {
        return false;
    }

    state.seen_paths.insert(normalized_path.clone());
    state.results.push(json!({
        "name": state.original_pattern,
        "type": "text",
        "path": normalized_path,
        "line": item.line_number,
        "text": item.line_text.trim(),
        "source": state.source,
    }));
    true
}

fn is_live_text_searchable_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [
        ".h", ".hh", ".hpp", ".hxx", ".c", ".cc", ".cpp", ".cxx", ".inl", ".ipp", ".cs", ".ini",
        ".json", ".uproject", ".uplugin",
    ]
    .iter()
    .any(|ext| lowered.ends_with(ext))
}

fn live_find_plugin_root(path: &str) -> Option<String> {
    let normalized = normalize_to_unix(path);
    let lowered = normalized.to_ascii_lowercase();
    let plugin_index = lowered.find("/plugins/")?;
    let after_plugins = plugin_index + "/plugins/".len();
    let tail = &normalized[after_plugins..];
    let next_sep = tail.find('/')?;
    Some(normalized[..after_plugins + next_sep].to_string())
}

fn open_engine_query_connection(
    state: Arc<AppState>,
    engine_db_path: Option<String>,
    label: &str,
) -> Result<Option<Arc<parking_lot::Mutex<rusqlite::Connection>>>> {
    let Some(engine_db_path) = engine_db_path.filter(|path| !path.trim().is_empty()) else {
        return Ok(None);
    };

    let engine_db_path = normalize_to_native(&engine_db_path);
    if !Path::new(&engine_db_path).is_file() {
        return Ok(None);
    }

    match state.get_shared_read_only_connection(&engine_db_path) {
        Ok(conn) => Ok(Some(conn)),
        Err(err) => {
            warn!("Failed to open Engine DB for {}: {}", label, err);
            Ok(None)
        }
    }
}

fn load_search_hot_index(
    state: &AppState,
    db_path: &str,
    label: &str,
) -> Option<Arc<query::search::SearchHotIndex>> {
    match state.get_search_hot_index(db_path) {
        Ok(index) => Some(index),
        Err(err) => {
            warn!("Failed to open search hot index for {} ({}): {}", db_path, label, err);
            None
        }
    }
}

fn load_navigation_hot_index(
    state: &AppState,
    db_path: &str,
    label: &str,
) -> Option<Arc<query::goto::NavigationHotIndex>> {
    match state.get_navigation_hot_index(db_path) {
        Ok(index) => Some(index),
        Err(err) => {
            warn!(
                "Failed to open navigation hot index for {} ({}): {}",
                db_path, label, err
            );
            None
        }
    }
}

fn load_usage_hot_index(
    state: &AppState,
    db_path: &str,
    label: &str,
) -> Option<Arc<query::usage::UsageHotIndex>> {
    match state.get_usage_hot_index(db_path) {
        Ok(index) => Some(index),
        Err(err) => {
            warn!("Failed to open usage hot index for {} ({}): {}", db_path, label, err);
            None
        }
    }
}

/// Convert a JSON array value into a Vec.
/// 将 JSON array value 转成 Vec。
fn value_array(value: Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// Extract the `results` array from an object response.
/// 从对象响应里提取 `results` 数组。
fn nested_results_array(value: &Value) -> Vec<Value> {
    value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Add a source marker to query result objects.
/// 给查询结果对象添加来源标记。
fn tag_source(items: &mut [Value], source: &str) {
    for item in items {
        if let Some(object) = item.as_object_mut() {
            object.entry("source").or_insert_with(|| json!(source));
        }
    }
}

/// Merge query results while keeping project results first and avoiding duplicates.
/// 合并查询结果，保持项目结果优先，并去重。
fn merge_query_results(target: &mut Vec<Value>, extra: Vec<Value>, limit: usize) {
    let mut seen = target
        .iter()
        .map(result_identity)
        .collect::<HashSet<String>>();

    for item in extra {
        if target.len() >= limit {
            break;
        }

        let identity = result_identity(&item);
        if seen.insert(identity) {
            target.push(item);
        }
    }
}

fn class_symbol_result_rank(item: &Value, pattern: &str) -> (u8, String, String, u8) {
    let query = pattern.trim().to_ascii_lowercase();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let class_name = item
        .get("class_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let module_name = item
        .get("module_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = item
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let source_rank = match item.get("source").and_then(Value::as_str).unwrap_or_default() {
        "project" => 0,
        "engine" => 1,
        _ => 2,
    };

    let rank = if name == query {
        0
    } else if !query.is_empty() && name.starts_with(&query) {
        1
    } else if !query.is_empty() && name.contains(&query) {
        2
    } else if !query.is_empty() && class_name.contains(&query) {
        3
    } else if !query.is_empty() && module_name.contains(&query) {
        4
    } else if !query.is_empty() && path.contains(&query) {
        5
    } else {
        9
    };

    (rank, name, path, source_rank)
}

fn dedupe_and_rank_class_symbol_results(results: &mut Vec<Value>, pattern: &str) {
    let mut seen = HashSet::new();
    results.retain(|item| {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        seen.insert(format!("{kind}\t{name}\t{path}"))
    });

    results.sort_by(|a, b| class_symbol_result_rank(a, pattern).cmp(&class_symbol_result_rank(b, pattern)));
}

/// Build a stable identity for de-duplicating merged query results.
/// 为合并查询结果构造稳定去重 key。
fn result_identity(item: &Value) -> String {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = item
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = item
        .get("line")
        .and_then(Value::as_i64)
        .unwrap_or_default();

    format!("{}:{}:{}", name, path, line)
}

// -----------------------------------------------------------------------------
// Scan handler
// -----------------------------------------------------------------------------

/// Scan a batch of files and save parsed symbols into SQLite.
/// 扫描一批文件，并把解析结果保存到 SQLite。
pub async fn handle_scan(state: &AppState, params: &Value) -> Result<Value> {
    let req: ScanRequest = convert_params(params)?;

    let db_path = req
        .files
        .first()
        .and_then(|file| file.db_path.clone())
        .ok_or_else(|| anyhow!("No DB path"))?;

    let db_path_native = normalize_to_native(&db_path);
    let conn = state.get_connection(&db_path_native)?;

    tokio::task::spawn_blocking(move || {
        let language = tree_sitter_unreal_cpp::LANGUAGE.into();
        let query = tree_sitter::Query::new(&language, scanner::QUERY_STR)?;
        let include_query = tree_sitter::Query::new(&language, scanner::INCLUDE_QUERY_STR)?;

        let results = req
            .files
            .into_iter()
            .filter_map(|input| {
                scanner::process_file(&input, &language, &query, &include_query).ok()
            })
            .collect::<Vec<_>>();

        let mut conn = conn.lock();
        db::save_to_db_incremental(&mut conn, &results, Arc::new(crate::types::StdoutReporter))?;

        Ok(json!(results.len()))
    })
    .await?
}

// -----------------------------------------------------------------------------
// Status handlers
// -----------------------------------------------------------------------------

/// Return server status.
/// 获取 server 当前状态。
pub async fn get_status(state: &AppState) -> Result<Value> {
    let projects = state.projects.lock();
    let clients = state.active_clients.lock();
    let exe_path = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string());
    let server_version = env!("CARGO_PKG_VERSION");

    Ok(json!({
        "status": "running",
        "protocol_version": SERVER_PROTOCOL_VERSION,
        "server_version": server_version,
        "build_id": format!("{}-p{}", server_version, SERVER_PROTOCOL_VERSION),
        "exe_path": exe_path,
        "active_projects": projects.keys().cloned().collect::<Vec<_>>(),
        "active_clients": clients.iter().copied().collect::<Vec<_>>(),
    }))
}

/// List registered projects.
/// 列出已注册工程。
pub async fn list_projects(state: &AppState) -> Result<Value> {
    let projects = state.projects.lock();

    let list = projects
        .iter()
        .map(|(root, ctx)| {
            json!({
                "root": root,
                "db_path": ctx.db_path,
                "cache_db_path": ctx.cache_db_path,
                "vcs_hash": ctx.vcs_hash,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!(list))
}

// -----------------------------------------------------------------------------
// Modify handlers
// -----------------------------------------------------------------------------

/// Add a module to a .uproject or .uplugin file.
/// 给 .uproject 或 .uplugin 添加模块。
pub async fn handle_modify_uproject_add_module(params: &Value) -> Result<Value> {
    let req: ModifyUprojectAddModuleRequest = convert_params(params)?;

    let result = tokio::task::spawn_blocking(move || {
        crate::edit::uproject::add_module(
            &req.file_path,
            &req.module_name,
            &req.module_type,
            &req.loading_phase,
        )
    })
    .await?;

    Ok(modify_result_to_json(result))
}

/// Add a module to a .Target.cs file.
/// 给 .Target.cs 添加模块。
pub async fn handle_modify_target_add_module(params: &Value) -> Result<Value> {
    let req: ModifyTargetAddModuleRequest = convert_params(params)?;

    let result = tokio::task::spawn_blocking(move || {
        crate::edit::target::add_module(&req.file_path, &req.module_name)
    })
    .await?;

    Ok(modify_result_to_json(result))
}

/// Convert modify result into public JSON shape.
/// 把修改结果转换成公开 JSON 结构。
fn modify_result_to_json(result: Result<()>) -> Value {
    match result {
        Ok(()) => serde_json::to_value(ModifyResult {
            success: true,
            message: None,
        })
        .unwrap_or_else(|_| json!({ "success": true })),

        Err(err) => serde_json::to_value(ModifyResult {
            success: false,
            message: Some(err.to_string()),
        })
        .unwrap_or_else(|_| json!({ "success": false, "message": err.to_string() })),
    }
}

// -----------------------------------------------------------------------------
// Asset query helpers
// -----------------------------------------------------------------------------

/// Get dependencies for a single asset by parsing the asset file directly.
/// 直接解析单个资产文件，获取它依赖的资源和父类。
fn get_asset_dependencies(project_root: &str, asset_path: &str) -> Result<Value> {
    if asset_path.starts_with("/Script/") {
        return Ok(json!({
            "dependencies": [],
            "parent_class": Value::Null,
        }));
    }

    let Some(file_path) = find_asset_file(project_root, asset_path) else {
        return Ok(json!({
            "dependencies": [],
            "parent_class": Value::Null,
        }));
    };

    let mut parser = crate::uasset::UAssetParser::new();
    parser.parse(&file_path)?;

    let mut deps = parser.imports;
    deps.sort();
    deps.dedup();

    Ok(json!({
        "dependencies": deps,
        "parent_class": parser.parent_class,
    }))
}

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------

fn query_request_label(request: &QueryRequest) -> &'static str {
    match request {
        QueryRequest::GetDiagnostics { .. } => "GetDiagnostics",
        QueryRequest::GetCompletions { .. } => "GetCompletions",
        QueryRequest::GetHover { .. } => "GetHover",
        QueryRequest::GetSignatureHelp { .. } => "GetSignatureHelp",
        QueryRequest::GotoDefinition { .. } => "GotoDefinition",
        QueryRequest::GotoImplementation { .. } => "GotoImplementation",
        QueryRequest::GetFileSymbols { .. } => "GetFileSymbols",
        QueryRequest::GetClassMembers { .. } => "GetClassMembers",
        QueryRequest::GetClassMembersRecursive { .. } => "GetClassMembersRecursive",
        QueryRequest::FindDerivedClasses { .. } => "FindDerivedClasses",
        QueryRequest::GetAssets => "GetAssets",
        QueryRequest::GetAssetIndexStatus => "GetAssetIndexStatus",
        QueryRequest::GetAssetUsages { .. } => "GetAssetUsages",
        QueryRequest::GetAssetUsageHints { .. } => "GetAssetUsageHints",
        QueryRequest::GetAssetDependencies { .. } => "GetAssetDependencies",
        QueryRequest::FastFind { .. } => "FastFind",
        QueryRequest::UnifiedLiveFind { .. } => "UnifiedLiveFind",
        QueryRequest::GlobalFind { .. } => "GlobalFind",
        QueryRequest::SearchCodeText { .. } => "SearchCodeText",
        QueryRequest::SearchSymbols { .. } => "SearchSymbols",
        QueryRequest::SearchClassSymbols { .. } => "SearchClassSymbols",
        QueryRequest::FindSymbolUsages { .. } => "FindSymbolUsages",
        QueryRequest::GetConfigData { .. } => "GetConfigData",
        _ => "Other",
    }
}

fn query_request_detail(request: &QueryRequest) -> Option<String> {
    match request {
        QueryRequest::GetDiagnostics { file_path, open_files, .. } => Some(format!(
            "file={}, overlays={}",
            short_path(file_path.as_deref()),
            open_files.len()
        )),
        QueryRequest::GetCompletions { file_path, .. } => {
            Some(format!("file={}", short_path(file_path.as_deref())))
        }
        QueryRequest::GetHover { file_path, .. } => {
            Some(format!("file={}", short_path(file_path.as_deref())))
        }
        QueryRequest::GetSignatureHelp { file_path, .. } => {
            Some(format!("file={}", short_path(file_path.as_deref())))
        }
        QueryRequest::GotoDefinition { file_path, .. } => {
            Some(format!("file={}", short_path(file_path.as_deref())))
        }
        QueryRequest::GotoImplementation { file_path, .. } => {
            Some(format!("file={}", short_path(file_path.as_deref())))
        }
        QueryRequest::SearchSymbols { pattern, limit, offset } => Some(format!(
            "pattern={} limit={} offset={}",
            pattern, limit, offset
        )),
        QueryRequest::SearchClassSymbols { pattern, limit, offset } => Some(format!(
            "pattern={} limit={} offset={}",
            pattern, limit, offset
        )),
        QueryRequest::FastFind { scope, .. } => Some(format!(
            "scope={}",
            scope.as_deref().unwrap_or("project")
        )),
        QueryRequest::UnifiedLiveFind {
            pattern,
            limit,
            offset,
            current_file,
            repeated_query,
        } => Some(format!(
            "pattern={} limit={} offset={} repeated={} file={}",
            pattern,
            limit,
            offset,
            repeated_query,
            short_path(current_file.as_deref())
        )),
        QueryRequest::GlobalFind { pattern, limit, offset } => Some(format!(
            "pattern={} limit={} offset={}",
            pattern, limit, offset
        )),
        QueryRequest::SearchCodeText {
            pattern,
            limit,
            offset,
            scope,
        } => Some(format!(
            "pattern={} limit={} offset={} scope={}",
            pattern,
            limit,
            offset,
            scope.as_deref().unwrap_or("project")
        )),
        QueryRequest::FindSymbolUsages { symbol_name, .. } => {
            Some(format!("symbol={}", symbol_name))
        }
        QueryRequest::FindDerivedClasses { base_class } => {
            Some(format!("base_class={}", base_class))
        }
        QueryRequest::GetClassMembers { class_name } => {
            Some(format!("class={}", class_name))
        }
        QueryRequest::GetAssetUsages { asset_path } => Some(format!("asset={}", asset_path)),
        QueryRequest::GetAssetUsageHints { names } => Some(format!("names={}", names.len())),
        QueryRequest::GetAssetDependencies { asset_path } => Some(format!("asset={}", asset_path)),
        QueryRequest::GetAssetIndexStatus => Some("project".to_string()),
        _ => None,
    }
}

fn short_path(path: Option<&str>) -> String {
    path.and_then(|value| value.rsplit('/').next().or_else(|| value.rsplit('\\').next()))
        .unwrap_or("-")
        .to_string()
}

fn record_query_perf(label: &'static str, elapsed: Duration, is_error: bool) {
    let lock = QUERY_PERF.get_or_init(|| {
        Mutex::new(QueryPerfWindow {
            started_at: Instant::now(),
            stats: HashMap::new(),
        })
    });

    let mut window = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    if window.started_at.elapsed() >= QUERY_PERF_WINDOW && !window.stats.is_empty() {
        flush_query_perf(&mut window);
    }

    let elapsed_ms = elapsed.as_millis();
    let stat = window.stats.entry(label).or_default();
    stat.count += 1;
    stat.total_ms += elapsed_ms;
    stat.max_ms = stat.max_ms.max(elapsed_ms);
    if is_error {
        stat.errors += 1;
    }
}

fn flush_query_perf(window: &mut QueryPerfWindow) {
    let mut parts = window
        .stats
        .iter()
        .map(|(label, stat)| {
            let avg_ms = if stat.count == 0 {
                0
            } else {
                stat.total_ms / stat.count as u128
            };

            format!(
                "{} count={} avg={}ms max={}ms errors={}",
                label, stat.count, avg_ms, stat.max_ms, stat.errors
            )
        })
        .collect::<Vec<_>>();

    parts.sort();

    info!(
        "Query perf window {} ms: {}",
        window.started_at.elapsed().as_millis(),
        parts.join(" | ")
    );

    window.started_at = Instant::now();
    window.stats.clear();
}

/// Ensure the persistent asset index exists for this project.
/// 确保当前项目的持久化资产索引已经初始化。
async fn ensure_asset_index_ready(state: Arc<AppState>, db_path_native: &str, project_root: &str) -> Result<()> {
    let db_path_native = db_path_native.to_string();
    let project_root = project_root.to_string();

    tokio::task::spawn_blocking(move || {
        let started_at = Instant::now();
        let mut conn = rusqlite::Connection::open(&db_path_native)?;
        db::init_db(&conn)?;

        if asset::asset_index_initialized(&conn)? {
            let _ = asset::populate_asset_graph_state(&state, &project_root, &conn);
            info!(
                project_root = %project_root,
                db_path = %db_path_native,
                elapsed_ms = started_at.elapsed().as_millis(),
                "background asset index skipped because it is already initialized"
            );
            return Ok(());
        }

        let reporter = Arc::new(crate::types::StdoutReporter);
        info!(
            project_root = %project_root,
            db_path = %db_path_native,
            "background asset index started"
        );
        let result = asset::refresh_asset_index(&mut conn, Path::new(&project_root), reporter);
        if result.is_ok() {
            let _ = asset::populate_asset_graph_state(&state, &project_root, &conn);
        }
        match &result {
            Ok(()) => info!(
                project_root = %project_root,
                elapsed_ms = started_at.elapsed().as_millis(),
                "background asset index finished"
            ),
            Err(err) => warn!(
                project_root = %project_root,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = %err,
                "background asset index failed"
            ),
        }
        result
    })
    .await?
}

struct SetupReadiness {
    needs_full_refresh: bool,
    needs_asset_index: bool,
}

/// Ensure SQLite database exists, version matches, and has required data.
/// 确保 SQLite 数据库存在、版本正确，并且不是空索引。
fn is_engine_root_path(path: &Path) -> bool {
    path.join("Engine/Source").is_dir() || path.join("Engine/Build/Build.version").is_file()
}

async fn ensure_database_ready(db_path_native: String, project_root: String) -> Result<SetupReadiness> {
    tokio::task::spawn_blocking(move || {
        let reinitialized = db::ensure_correct_version(&db_path_native).unwrap_or(false);

        if reinitialized {
            info!(
                project_root = %project_root,
                db_path = %db_path_native,
                "setup requires full refresh because database was reinitialized"
            );
            return Ok(SetupReadiness {
                needs_full_refresh: true,
                needs_asset_index: false,
            });
        }

        let conn = rusqlite::Connection::open(&db_path_native)?;
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap_or(0);
        let class_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM classes", [], |row| row.get(0))
            .unwrap_or(0);

        if file_count == 0 || class_count == 0 {
            info!(
                project_root = %project_root,
                db_path = %db_path_native,
                file_count = file_count,
                class_count = class_count,
                "setup requires full refresh because code index is empty"
            );
            return Ok(SetupReadiness {
                needs_full_refresh: true,
                needs_asset_index: false,
            });
        }

        if is_engine_root_path(Path::new(&project_root)) {
            info!(
                project_root = %project_root,
                db_path = %db_path_native,
                file_count = file_count,
                class_count = class_count,
                "setup ready for engine database without asset index"
            );
            return Ok(SetupReadiness {
                needs_full_refresh: false,
                needs_asset_index: false,
            });
        }

        let asset_ready = asset::asset_index_initialized(&conn).unwrap_or(false);
        info!(
            project_root = %project_root,
            db_path = %db_path_native,
            file_count = file_count,
            class_count = class_count,
            asset_ready = asset_ready,
            "setup checked project database readiness"
        );
        Ok(SetupReadiness {
            needs_full_refresh: false,
            needs_asset_index: !asset_ready,
        })
    })
    .await?
}

/// Remove cached database connections before refresh/setup.
/// setup 或 refresh 前移除旧 DB 连接缓存。
fn drop_db_connections(state: &AppState, db_path_native: &str, cache_db_path_unix: Option<&str>) {
    let mut conns = state.connections.lock();
    conns.remove(db_path_native);
    drop(conns);
    state.read_only_connections.lock().remove(db_path_native);
    state.invalidate_search_hot_index(db_path_native);
    state.invalidate_navigation_hot_index(db_path_native);
    state.invalidate_usage_hot_index(db_path_native);

    if let Some(cache_path) = cache_db_path_unix {
        let mut cache_conns = state.persistent_cache_connections.lock();
        cache_conns.remove(&normalize_to_native(cache_path));
    }
}

/// Insert or update project context for refresh.
/// refresh 前插入或更新工程上下文。
fn upsert_refresh_project_context(
    state: &AppState,
    req: &mut RefreshRequest,
    root_key: &str,
) -> Result<String> {
    let mut projects = state.projects.lock();

    if let Some(db_path) = &req.db_path {
        let db_path_unix = normalize_to_unix(db_path);

        projects.insert(
            root_key.to_string(),
            ProjectContext {
                db_path: db_path_unix.clone(),
                cache_db_path: req.cache_db_path.as_ref().map(|p| normalize_to_unix(p)),
                vcs_hash: req.vcs_hash.clone(),
                last_refresh_at: Instant::now(),
            },
        );

        return Ok(db_path_unix);
    }

    let Some(ctx) = projects.get_mut(root_key) else {
        return Err(anyhow!("Project not found: {}", root_key));
    };

    ctx.vcs_hash = req.vcs_hash.clone();

    if let Some(cache_path) = &req.cache_db_path {
        ctx.cache_db_path = Some(normalize_to_unix(cache_path));
    }

    Ok(ctx.db_path.clone())
}

/// Clear completion cache after project refresh.
/// 工程刷新后清空补全缓存。
fn clear_completion_cache(state: &AppState, root_key: &str) {
    let cache = state.get_completion_cache(root_key);
    cache.lock().clear();
    info!("Completion cache cleared after refresh: {}", root_key);
}

/// Check whether a refresh is active for the project.
/// 判断工程是否正在 refresh。
fn is_refreshing(state: &AppState, root_key: &str) -> bool {
    state.active_refreshes.lock().contains(root_key)
}

/// Guard for active_refreshes.
/// active_refreshes 的自动清理保护对象。
struct RefreshGuard<'a> {
    state: &'a AppState,
    root_key: String,
}

impl<'a> RefreshGuard<'a> {
    /// Create guard or return early if refresh is already active.
    /// 创建 refresh guard；如果已经在刷新则直接返回错误。
    fn try_new(state: &'a AppState, root_key: String) -> Result<Self> {
        let mut active = state.active_refreshes.lock();

        if !active.insert(root_key.clone()) {
            return Err(anyhow!("Refresh already in progress"));
        }

        Ok(Self { state, root_key })
    }
}

impl Drop for RefreshGuard<'_> {
    fn drop(&mut self) {
        self.state.active_refreshes.lock().remove(&self.root_key);
    }
}

/// Get registered project context by root key.
/// 根据 root_key 获取工程上下文。
fn get_project_context(state: &AppState, root_key: &str) -> Result<ProjectContext> {
    let projects = state.projects.lock();

    projects
        .get(root_key)
        .cloned()
        .ok_or_else(|| anyhow!("Project not found: {}", root_key))
}

/// Locate an asset file from an Unreal /Game path.
/// 根据 Unreal /Game 路径定位真实资产文件。
fn find_asset_file(project_root: &str, asset_path: &str) -> Option<PathBuf> {
    let root = PathBuf::from(normalize_to_native(project_root));
    let relative = asset_path.replacen("/Game/", "Content/", 1);
    let basename = relative.rsplit('/').next().unwrap_or("");

    let candidates = [
        format!("{}.uasset", basename),
        format!("{}.umap", basename),
    ];

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(name.as_ref(), "Intermediate" | "Binaries" | "Build" | "Saved")
        })
        .build();

    for entry in walker.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy();

        if !candidates.iter().any(|candidate| candidate == name.as_ref()) {
            continue;
        }

        let normalized = entry.path().to_string_lossy().replace('\\', "/");

        if normalized.contains(&relative) {
            return Some(entry.path().to_path_buf());
        }
    }

    None
}
