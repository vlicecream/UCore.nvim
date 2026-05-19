//! Global find search rules.
//! 全局搜索规则。
//!
//! `GlobalFind` is the single backend contract for `:UCore find` / `gf`.
//! Lua sends only a semantic request: `pattern + limit + offset`.
//! This Rust module owns SQL, DB schema details, text scanning, ranking,
//! pagination, and future index changes.
//! `GlobalFind` 是 `:UCore find` / `gf` 的统一后端契约。Lua 只发送
//! `pattern + limit + offset` 语义请求；SQL、数据库结构、文本扫描、
//! 排序、分页以及后续索引变更都由 Rust 侧负责。
//!
//! Live find ranking contract:
//! 1. project class-like results (`class`, `struct`, `enum`, `UCLASS`,
//!    `USTRUCT`, `UENUM`);
//! 2. project file basename/path results;
//! 3. other project symbols such as functions, methods, properties, members;
//! 4. project code text line matches;
//! 5. Engine results appended later and ranked after project results;
//! 6. loose picker-side fuzzy only breaks ties inside the staged results.
//!
//! `FastFind` intentionally omits code text so live search can show class/file
//! results quickly. Lua starts `SearchCodeText` as a separate project-only stage
//! and starts Engine `FastFind` as the final low-priority stage.
//! 实时搜索规则：Project 类结果优先，其次文件名/路径，再是普通 symbol，
//! 然后才是 Project 代码正文；Engine 结果后补并整体排在 Project 后面。
//! `FastFind` 不查正文，正文由 Lua 另起 `SearchCodeText` 阶段追加。

use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tracing::info;

use crate::db::ensure_search_projections;
use crate::db::project_path::PATH_CTE;
use crate::db::text;

const FIND_FUZZY_FALLBACK_LIMIT: usize = 256;
static FAST_FIND_LOG_ENABLED: OnceLock<bool> = OnceLock::new();

#[derive(Clone)]
struct SymbolHotEntry {
    name: String,
    kind: String,
    owner_name: Option<String>,
    path: String,
    line_number: Option<i64>,
    module_name: Option<String>,
    name_lc: String,
    compact_name: String,
    owner_name_lc: String,
    module_name_lc: String,
    kind_rank: i64,
    is_class_like: bool,
}

#[derive(Clone)]
struct FileHotEntry {
    basename: String,
    path: String,
    module_name: Option<String>,
    module_root: Option<String>,
    basename_lc: String,
    path_lc: String,
}

pub struct SearchHotIndex {
    symbols: Vec<SymbolHotEntry>,
    files: Vec<FileHotEntry>,
    symbol_exact: HashMap<String, Vec<usize>>,
    symbol_compact_exact: HashMap<String, Vec<usize>>,
    symbol_name_prefix: Vec<(String, usize)>,
    symbol_compact_prefix: Vec<(String, usize)>,
    symbol_owner_exact: HashMap<String, Vec<usize>>,
    symbol_owner_prefix: Vec<(String, usize)>,
    symbol_module_exact: HashMap<String, Vec<usize>>,
    symbol_module_prefix: Vec<(String, usize)>,
    file_basename_exact: HashMap<String, Vec<usize>>,
    file_path_exact: HashMap<String, Vec<usize>>,
    file_basename_prefix: Vec<(String, usize)>,
    file_path_prefix: Vec<(String, usize)>,
}

fn fast_find_log_enabled() -> bool {
    *FAST_FIND_LOG_ENABLED.get_or_init(|| {
        std::env::var("UCORE_FAST_FIND_LOG")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false)
    })
}

impl SearchHotIndex {
    fn query(&self, pattern: &str, limit: usize, offset: usize, class_only: bool) -> Vec<Value> {
        let target = offset.saturating_add(limit).clamp(1, 10_000);
        let query_text = pattern.trim();
        if query_text.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let query = query_text.to_ascii_lowercase();
        let prefix = query.clone();
        let identifier_query = is_identifier_query(query_text);

        self.query_symbols_into(
            &mut results,
            &mut seen,
            query_text,
            &query,
            &prefix,
            identifier_query,
            class_only,
            target,
        );

        if !class_only && !identifier_query {
            self.merge_file_exact(&mut results, &mut seen, &query, target);
            self.merge_file_path_exact(&mut results, &mut seen, &query, target);
            self.merge_file_prefix(
                &mut results,
                &mut seen,
                &self.file_basename_prefix,
                &prefix,
                target,
            );
            self.merge_file_prefix(
                &mut results,
                &mut seen,
                &self.file_path_prefix,
                &prefix,
                target,
            );
        }

        results.into_iter().skip(offset).take(limit).collect()
    }

    fn query_symbols_only(
        &self,
        pattern: &str,
        limit: usize,
        offset: usize,
        class_only: bool,
    ) -> Vec<Value> {
        let target = offset.saturating_add(limit).clamp(1, 10_000);
        let query_text = pattern.trim();
        if query_text.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let query = query_text.to_ascii_lowercase();
        let prefix = query.clone();
        let identifier_query = is_identifier_query(query_text);
        self.query_symbols_into(
            &mut results,
            &mut seen,
            query_text,
            &query,
            &prefix,
            identifier_query,
            class_only,
            target,
        );
        results.into_iter().skip(offset).take(limit).collect()
    }

    fn query_symbols_into(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        query_text: &str,
        query: &str,
        prefix: &str,
        identifier_query: bool,
        class_only: bool,
        target: usize,
    ) {
        self.merge_symbol_exact(out, seen, query, class_only, target);
        if identifier_query {
            let compact = compact_identifier(query_text);
            self.merge_symbol_compact_exact(out, seen, &compact, class_only, target);
            self.merge_symbol_prefix(
                out,
                seen,
                &self.symbol_compact_prefix,
                &compact,
                class_only,
                target,
            );
        }
        self.merge_symbol_prefix(
            out,
            seen,
            &self.symbol_name_prefix,
            prefix,
            class_only,
            target,
        );

        if !identifier_query {
            self.merge_symbol_exact_map(
                out,
                seen,
                &self.symbol_owner_exact,
                query,
                class_only,
                target,
            );
            self.merge_symbol_prefix(
                out,
                seen,
                &self.symbol_owner_prefix,
                prefix,
                class_only,
                target,
            );
            self.merge_symbol_exact_map(
                out,
                seen,
                &self.symbol_module_exact,
                query,
                class_only,
                target,
            );
            self.merge_symbol_prefix(
                out,
                seen,
                &self.symbol_module_prefix,
                prefix,
                class_only,
                target,
            );
        }
    }

    fn merge_symbol_exact(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        key: &str,
        class_only: bool,
        limit: usize,
    ) {
        if let Some(indices) = self.symbol_exact.get(key) {
            self.merge_symbol_indices(out, seen, indices, class_only, limit);
        }
    }

    fn merge_symbol_compact_exact(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        key: &str,
        class_only: bool,
        limit: usize,
    ) {
        if let Some(indices) = self.symbol_compact_exact.get(key) {
            self.merge_symbol_indices(out, seen, indices, class_only, limit);
        }
    }

    fn merge_symbol_exact_map(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        map: &HashMap<String, Vec<usize>>,
        key: &str,
        class_only: bool,
        limit: usize,
    ) {
        if let Some(indices) = map.get(key) {
            self.merge_symbol_indices(out, seen, indices, class_only, limit);
        }
    }

    fn merge_symbol_indices(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        indices: &[usize],
        class_only: bool,
        limit: usize,
    ) {
        for &index in indices {
            if out.len() >= limit {
                break;
            }
            let entry = &self.symbols[index];
            if class_only && !entry.is_class_like {
                continue;
            }
            let value = symbol_hot_entry_to_value(entry);
            let key = find_result_identity(&value);
            if seen.insert(key) {
                out.push(value);
            }
        }
    }

    fn merge_symbol_prefix(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        sorted: &[(String, usize)],
        prefix: &str,
        class_only: bool,
        limit: usize,
    ) {
        if prefix.is_empty() || out.len() >= limit {
            return;
        }

        let mut index = lower_bound_prefix(sorted, prefix);
        while index < sorted.len() && sorted[index].0.starts_with(prefix) {
            if out.len() >= limit {
                break;
            }
            let entry = &self.symbols[sorted[index].1];
            if !class_only || entry.is_class_like {
                let value = symbol_hot_entry_to_value(entry);
                let key = find_result_identity(&value);
                if seen.insert(key) {
                    out.push(value);
                }
            }
            index += 1;
        }
    }

    fn merge_file_exact(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        key: &str,
        limit: usize,
    ) {
        if let Some(indices) = self.file_basename_exact.get(key) {
            self.merge_file_indices(out, seen, indices, limit);
        }
    }

    fn merge_file_path_exact(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        key: &str,
        limit: usize,
    ) {
        if let Some(indices) = self.file_path_exact.get(key) {
            self.merge_file_indices(out, seen, indices, limit);
        }
    }

    fn merge_file_indices(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        indices: &[usize],
        limit: usize,
    ) {
        for &index in indices {
            if out.len() >= limit {
                break;
            }
            let value = file_hot_entry_to_value(&self.files[index]);
            let key = find_result_identity(&value);
            if seen.insert(key) {
                out.push(value);
            }
        }
    }

    fn merge_file_prefix(
        &self,
        out: &mut Vec<Value>,
        seen: &mut HashSet<String>,
        sorted: &[(String, usize)],
        prefix: &str,
        limit: usize,
    ) {
        if prefix.is_empty() || out.len() >= limit {
            return;
        }

        let mut index = lower_bound_prefix(sorted, prefix);
        while index < sorted.len() && sorted[index].0.starts_with(prefix) {
            if out.len() >= limit {
                break;
            }
            let value = file_hot_entry_to_value(&self.files[sorted[index].1]);
            let key = find_result_identity(&value);
            if seen.insert(key) {
                out.push(value);
            }
            index += 1;
        }
    }
}

fn lower_bound_prefix(sorted: &[(String, usize)], prefix: &str) -> usize {
    let mut left = 0usize;
    let mut right = sorted.len();
    while left < right {
        let mid = (left + right) / 2;
        if sorted[mid].0.as_str() < prefix {
            left = mid + 1;
        } else {
            right = mid;
        }
    }
    left
}

fn symbol_hot_entry_to_value(entry: &SymbolHotEntry) -> Value {
    json!({
        "name": entry.name,
        "type": entry.kind,
        "class_name": entry.owner_name,
        "path": entry.path,
        "line": entry.line_number,
        "module_name": entry.module_name,
    })
}

fn file_hot_entry_to_value(entry: &FileHotEntry) -> Value {
    json!({
        "name": entry.basename,
        "type": "file",
        "path": entry.path,
        "line": 1,
        "module_name": entry.module_name,
        "module_root": entry.module_root,
    })
}

pub fn build_search_hot_index(conn: &Connection) -> anyhow::Result<SearchHotIndex> {
    ensure_search_projections(conn)?;

    let mut symbols = Vec::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT
            name,
            kind,
            owner_name,
            path,
            line_number,
            module_name,
            name_lc,
            compact_name,
            owner_name_lc,
            module_name_lc,
            kind_rank,
            is_class_like
        FROM search_symbols
        ORDER BY kind_rank ASC, name_lc ASC, path_lc ASC
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SymbolHotEntry {
            name: row.get(0)?,
            kind: row.get(1)?,
            owner_name: row.get(2)?,
            path: normalize_path(&row.get::<_, String>(3)?),
            line_number: row.get(4)?,
            module_name: row.get(5)?,
            name_lc: row.get(6)?,
            compact_name: row.get(7)?,
            owner_name_lc: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            module_name_lc: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            kind_rank: row.get(10)?,
            is_class_like: row.get::<_, i64>(11)? != 0,
        })
    })?;
    for row in rows {
        symbols.push(row?);
    }

    let mut files = Vec::new();
    let mut stmt = conn.prepare(&format!(
        r#"
        {}
        SELECT
            sf.basename,
            sf.path,
            sf.module_name,
            rd.full_path AS module_root,
            sf.basename_lc,
            sf.path_lc
        FROM search_files sf
        LEFT JOIN modules m ON sf.module_id = m.id
        LEFT JOIN dir_paths rd ON m.root_directory_id = rd.id
        WHERE lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
        ORDER BY sf.basename_lc ASC, sf.path_lc ASC
        "#,
        PATH_CTE
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(FileHotEntry {
            basename: row.get(0)?,
            path: normalize_path(&row.get::<_, String>(1)?),
            module_name: row.get(2)?,
            module_root: row.get::<_, Option<String>>(3)?.map(|p| normalize_path(&p)),
            basename_lc: row.get(4)?,
            path_lc: row.get(5)?,
        })
    })?;
    for row in rows {
        files.push(row?);
    }

    let mut symbol_exact = HashMap::<String, Vec<usize>>::new();
    let mut symbol_compact_exact = HashMap::<String, Vec<usize>>::new();
    let mut symbol_name_prefix = Vec::with_capacity(symbols.len());
    let mut symbol_compact_prefix = Vec::with_capacity(symbols.len());
    let mut symbol_owner_exact = HashMap::<String, Vec<usize>>::new();
    let mut symbol_owner_prefix = Vec::new();
    let mut symbol_module_exact = HashMap::<String, Vec<usize>>::new();
    let mut symbol_module_prefix = Vec::new();
    for (index, entry) in symbols.iter().enumerate() {
        symbol_exact.entry(entry.name_lc.clone()).or_default().push(index);
        if !entry.compact_name.is_empty() {
            symbol_compact_exact
                .entry(entry.compact_name.clone())
                .or_default()
                .push(index);
            symbol_compact_prefix.push((entry.compact_name.clone(), index));
        }
        if !entry.owner_name_lc.is_empty() {
            symbol_owner_exact
                .entry(entry.owner_name_lc.clone())
                .or_default()
                .push(index);
            symbol_owner_prefix.push((entry.owner_name_lc.clone(), index));
        }
        if !entry.module_name_lc.is_empty() {
            symbol_module_exact
                .entry(entry.module_name_lc.clone())
                .or_default()
                .push(index);
            symbol_module_prefix.push((entry.module_name_lc.clone(), index));
        }
        symbol_name_prefix.push((entry.name_lc.clone(), index));
    }
    symbol_name_prefix.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| symbols[left.1].kind_rank.cmp(&symbols[right.1].kind_rank))
            .then_with(|| symbols[left.1].path.cmp(&symbols[right.1].path))
    });
    symbol_compact_prefix.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| symbols[left.1].kind_rank.cmp(&symbols[right.1].kind_rank))
            .then_with(|| symbols[left.1].path.cmp(&symbols[right.1].path))
    });
    symbol_owner_prefix.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| symbols[left.1].kind_rank.cmp(&symbols[right.1].kind_rank))
            .then_with(|| symbols[left.1].path.cmp(&symbols[right.1].path))
    });
    symbol_module_prefix.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| symbols[left.1].kind_rank.cmp(&symbols[right.1].kind_rank))
            .then_with(|| symbols[left.1].path.cmp(&symbols[right.1].path))
    });

    let mut file_basename_exact = HashMap::<String, Vec<usize>>::new();
    let mut file_path_exact = HashMap::<String, Vec<usize>>::new();
    let mut file_basename_prefix = Vec::with_capacity(files.len());
    let mut file_path_prefix = Vec::with_capacity(files.len());
    for (index, entry) in files.iter().enumerate() {
        file_basename_exact
            .entry(entry.basename_lc.clone())
            .or_default()
            .push(index);
        file_path_exact.entry(entry.path_lc.clone()).or_default().push(index);
        file_basename_prefix.push((entry.basename_lc.clone(), index));
        file_path_prefix.push((entry.path_lc.clone(), index));
    }
    file_basename_prefix.sort_unstable_by(|left, right| {
        left.0.cmp(&right.0).then_with(|| files[left.1].path.cmp(&files[right.1].path))
    });
    file_path_prefix.sort_unstable_by(|left, right| {
        left.0.cmp(&right.0).then_with(|| files[left.1].basename.cmp(&files[right.1].basename))
    });

    Ok(SearchHotIndex {
        symbols,
        files,
        symbol_exact,
        symbol_compact_exact,
        symbol_name_prefix,
        symbol_compact_prefix,
        symbol_owner_exact,
        symbol_owner_prefix,
        symbol_module_exact,
        symbol_module_prefix,
        file_basename_exact,
        file_path_exact,
        file_basename_prefix,
        file_path_prefix,
    })
}

pub fn fast_find_with_hot_index(
    conn: &Connection,
    hot_index: Option<&SearchHotIndex>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    hybrid_fast_find(conn, hot_index, pattern, limit, offset, false)
}

pub fn search_symbols_with_hot_index(
    conn: &Connection,
    hot_index: Option<&SearchHotIndex>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    let pattern = pattern.trim();
    let limit = limit.clamp(1, 10_000);
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit);
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    if let Some(index) = hot_index {
        merge_bucket_results(
            &mut results,
            &mut seen,
            index.query_symbols_only(pattern, target, 0, false),
            target,
        );
    }
    if results.len() < target {
        merge_bucket_results(
            &mut results,
            &mut seen,
            bucketed_symbol_results(conn, pattern, target, false)?,
            target,
        );
    }

    let page = results.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    Ok(json!(page))
}

pub fn search_class_symbols_with_hot_index(
    conn: &Connection,
    hot_index: Option<&SearchHotIndex>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    hybrid_fast_find(conn, hot_index, pattern, limit, offset, true)
}

pub fn global_find_with_hot_index(
    conn: &Connection,
    hot_index: Option<&SearchHotIndex>,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    let pattern = pattern.trim();
    let limit = limit.clamp(1, 500);
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit);
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    if let Some(index) = hot_index {
        merge_bucket_results(&mut results, &mut seen, index.query(pattern, target, 0, false), target);
    }

    if results.len() < target {
        let db_results = hybrid_fast_find(conn, None, pattern, target, 0, false)?
            .as_array()
            .cloned()
            .unwrap_or_default();
        merge_bucket_results(&mut results, &mut seen, db_results, target);
    }

    if results.len() < target {
        merge_bucket_results_value(
            &mut results,
            &mut seen,
            search_text_for_global(conn, pattern, target)?,
            target,
        );
    }

    let page = results.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    Ok(json!(page))
}

fn hybrid_fast_find(
    conn: &Connection,
    hot_index: Option<&SearchHotIndex>,
    pattern: &str,
    limit: usize,
    offset: usize,
    class_only: bool,
) -> anyhow::Result<Value> {
    let pattern = pattern.trim();
    let limit = limit.clamp(1, if class_only { 10_000 } else { 500 });
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return if class_only {
            list_class_symbols(conn, limit, offset)
        } else {
            list_symbols(conn, limit, offset)
        };
    }

    let target = offset.saturating_add(limit);
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    if let Some(index) = hot_index {
        merge_bucket_results(&mut results, &mut seen, index.query(pattern, target, 0, class_only), target);
    }

    if results.len() < target {
        let db_results = if class_only {
            bucketed_symbol_results(conn, pattern, target, true)?
        } else {
            let mut db_results = bucketed_symbol_results(conn, pattern, target, false)?;
            let identifier_query = is_identifier_query(pattern);
            if !identifier_query {
                let mut seen_db = db_results
                    .iter()
                    .map(find_result_identity)
                    .collect::<HashSet<_>>();
                merge_bucket_results(
                    &mut db_results,
                    &mut seen_db,
                    bucketed_file_results(conn, pattern, target)?,
                    target,
                );
            }
            let should_run_fallback =
                db_results.len() < target && !has_strong_explicit_type_match(&db_results, pattern);
            if should_run_fallback {
                let mut seen_db = db_results
                    .iter()
                    .map(find_result_identity)
                    .collect::<HashSet<_>>();
                merge_bucket_results_value(
                    &mut db_results,
                    &mut seen_db,
                    search_symbols_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
                    target,
                );
                if !identifier_query {
                    merge_bucket_results_value(
                        &mut db_results,
                        &mut seen_db,
                        search_files_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
                        target,
                    );
                }
            }
            db_results
        };
        merge_bucket_results(&mut results, &mut seen, db_results, target);
    }

    if !class_only {
        suppress_broad_class_contains_when_exact_type_exists(&mut results, pattern);
    }

    let page = results.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    Ok(json!(page))
}

/// Search indexed symbols with database-side ranking and pagination.
/// 使用数据库侧排序和分页搜索已索引的 symbol。
///
/// Ranking intentionally favors human "global find" expectations inside the
/// symbol bucket:
/// - exact symbol names first;
/// - then symbol name prefixes;
/// - then continuous symbol substrings;
/// - then owner class / module / path matches;
/// - only after those should picker-side fuzzy matching matter.
///
/// This prevents a query such as `death` from being dominated by loose
/// `d ... e ... a ... t ... h` fuzzy results before real `Death*` symbols.
/// 这里描述的是 symbol 桶内部排序：完全匹配、前缀匹配、连续子串优先，
/// 再看所属 class/module/path，最后才让前端 fuzzy 参与。
pub fn search_symbols(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    ensure_search_projections(conn)?;
    let pattern = pattern.trim();

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit).clamp(1, 10_000);
    let results = bucketed_symbol_results(conn, pattern, target, false)?;
    let page = results
        .into_iter()
        .skip(offset.min(1_000_000))
        .take(limit.clamp(1, 10_000))
        .collect::<Vec<_>>();

    Ok(json!(page))
}

/// Return indexed symbols for interactive picker-side fuzzy filtering.
/// 返回索引符号，供前端 picker 做本地模糊过滤。
fn list_symbols(conn: &Connection, limit: usize, offset: usize) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 10_000) as i64;
    let offset = offset.min(1_000_000) as i64;

    let sql = r#"
        SELECT
            name,
            kind,
            owner_name,
            path,
            line_number,
            module_name
        FROM search_symbols
        WHERE name NOT LIKE '(%'
        ORDER BY name_lc ASC, path_lc ASC
        LIMIT ? OFFSET ?
        "#;

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "type": row.get::<_, String>(1)?,
            "class_name": row.get::<_, Option<String>>(2)?,
            "path": normalize_path(&row.get::<_, String>(3)?),
            "line": row.get::<_, Option<i64>>(4)?,
            "module_name": row.get::<_, Option<String>>(5)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

/// Unified global find for symbols, files, and code text.
/// 统一搜索 symbol、文件名/路径和代码文本。
///
/// Unified find used by non-live callers. Live `gf` uses staged requests
/// (`FastFind`, `SearchCodeText`, then Engine `FastFind`) and the UI applies the
/// final bucket order: Classes > Files > Symbols > Text > Engine. This function
/// still keeps a stable backend order for broad one-shot searches.
/// 非实时入口使用这个统一查询；实时 `gf` 走分阶段请求，并由 UI 应用最终桶排序：
/// Classes > Files > Symbols > Text > Engine。这里仍保留一次性搜索的稳定后端顺序。
pub fn global_find(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    let pattern = pattern.trim();
    let limit = limit.clamp(1, 500);
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit);
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    merge_bucket_results(
        &mut results,
        &mut seen,
        bucketed_symbol_results(conn, pattern, target.max(limit), false)?,
        target.max(limit),
    );
    merge_bucket_results(
        &mut results,
        &mut seen,
        search_files_for_global(conn, pattern, target.max(limit))?,
        target.max(limit),
    );

    if results.len() < target {
        merge_bucket_results_value(
            &mut results,
            &mut seen,
            search_symbols_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
            target,
        );
        merge_bucket_results_value(
            &mut results,
            &mut seen,
            search_files_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
            target,
        );
    }

    if results.len() < target {
        merge_bucket_results_value(
            &mut results,
            &mut seen,
            search_text_for_global(conn, pattern, target)?,
            target,
        );
    }

    let page = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    Ok(json!(page))
}

/// Fast first-stage live find.
///
/// This returns only class/symbol rows and file rows. Code text is deliberately
/// excluded because scanning source files can block the first screen. Lua runs
/// `SearchCodeText` in parallel as a project-only append stage, and queries
/// Engine with a separate low-priority `FastFind` request.
/// 实时搜索第一阶段：只返回 class/symbol 和 file，不扫代码正文。代码正文由
/// Lua 并行追加，Engine 另走低优先级 `FastFind`。
pub fn fast_find(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    ensure_search_projections(conn)?;
    let pattern = pattern.trim();
    let limit = limit.clamp(1, 500);
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit);
    let mut results = bucketed_symbol_results(conn, pattern, target.max(limit), false)?;
    let identifier_query = is_identifier_query(pattern);

    if !identifier_query {
        let mut seen = results
            .iter()
            .map(find_result_identity)
            .collect::<std::collections::HashSet<_>>();
        merge_bucket_results(
            &mut results,
            &mut seen,
            search_files_for_global(conn, pattern, target.max(limit))?,
            target.max(limit),
        );
    }

    let should_run_fallback = results.len() < target && !has_strong_explicit_type_match(&results, pattern);
    if should_run_fallback {
        let mut seen = results
            .iter()
            .map(find_result_identity)
            .collect::<std::collections::HashSet<_>>();
        merge_bucket_results_value(
            &mut results,
            &mut seen,
            search_symbols_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
            target.max(limit),
        );
        if !identifier_query {
            merge_bucket_results_value(
                &mut results,
                &mut seen,
                search_files_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
                target.max(limit),
            );
        }
    }

    suppress_broad_class_contains_when_exact_type_exists(&mut results, pattern);

    let page = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    Ok(json!(page))
}

fn list_class_symbols(conn: &Connection, limit: usize, offset: usize) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 10_000) as i64;
    let offset = offset.min(1_000_000) as i64;

    let sql = r#"
        SELECT
            name,
            kind,
            owner_name,
            path,
            line_number,
            module_name
        FROM search_symbols
        WHERE is_class_like = 1
          AND name NOT LIKE '(%'
        ORDER BY name_lc ASC, path_lc ASC
        LIMIT ? OFFSET ?
        "#;

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "type": row.get::<_, String>(1)?,
            "class_name": row.get::<_, Option<String>>(2)?,
            "path": normalize_path(&row.get::<_, String>(3)?),
            "line": row.get::<_, Option<i64>>(4)?,
            "module_name": row.get::<_, Option<String>>(5)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

pub fn search_class_symbols(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    ensure_search_projections(conn)?;
    let pattern = pattern.trim();

    if pattern.is_empty() {
        return list_class_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit).clamp(1, 10_000);
    let results = bucketed_symbol_results(conn, pattern, target, true)?;
    let page = results
        .into_iter()
        .skip(offset.min(1_000_000))
        .take(limit.clamp(1, 10_000))
        .collect::<Vec<_>>();

    Ok(json!(page))
}

fn is_class_like_symbol_type(symbol_type: &str) -> bool {
    matches!(
        symbol_type,
        "class" | "struct" | "enum" | "UCLASS" | "USTRUCT" | "UENUM" | "UINTERFACE"
    )
}

fn is_identifier_query(query: &str) -> bool {
    let mut chars = query.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }

    chars.all(|ch| ch == '_' || ch == ':' || ch.is_ascii_alphanumeric())
}

fn is_explicit_type_query(query: &str) -> bool {
    if !is_identifier_query(query) {
        return false;
    }

    if query.contains("::") {
        return true;
    }

    query.chars().next().map(|ch| ch.is_ascii_uppercase()).unwrap_or(false)
        || looks_like_unreal_type_query(query)
}

fn looks_like_unreal_type_query(query: &str) -> bool {
    let query = query.trim();
    if query.len() < 8 || query.contains('_') || query.contains("::") || !is_identifier_query(query) {
        return false;
    }

    matches!(
        query.chars().next().map(|ch| ch.to_ascii_lowercase()),
        Some('u' | 'a' | 'f' | 'e' | 'i')
    )
}

fn compact_identifier(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn has_strong_explicit_type_match(results: &[Value], pattern: &str) -> bool {
    if !is_explicit_type_query(pattern.trim()) {
        return false;
    }

    let compact_query = compact_identifier(pattern.trim());
    if compact_query.is_empty() {
        return false;
    }

    results.iter().any(|item| {
        let symbol_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if !is_class_like_symbol_type(symbol_type) {
            return false;
        }

        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let compact_name = compact_identifier(name);
        !compact_name.is_empty() && compact_name.contains(&compact_query)
    })
}

fn suppress_broad_class_contains_when_exact_type_exists(results: &mut Vec<Value>, pattern: &str) {
    let query = pattern.trim().to_ascii_lowercase();
    if query.is_empty() || !is_explicit_type_query(pattern.trim()) {
        return;
    }

    let has_exact_type_match = results.iter().any(|item| {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let symbol_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        is_class_like_symbol_type(symbol_type) && name.eq_ignore_ascii_case(&query)
    });

    if !has_exact_type_match {
        return;
    }

    let before = results.len();
    results.retain(|item| {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        let symbol_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if is_class_like_symbol_type(symbol_type) {
            return name.eq_ignore_ascii_case(&query);
        }

        if symbol_type.eq_ignore_ascii_case("file") {
            return true;
        }

        let owner = item.get("class_name").and_then(Value::as_str).unwrap_or_default();
        if owner.eq_ignore_ascii_case(&query) && !name.eq_ignore_ascii_case(&query) {
            return false;
        }

        name.eq_ignore_ascii_case(&query)
    });

    if fast_find_log_enabled() {
        let after = results.len();
        info!(
            target: "ucore::fast_find",
            "FastFind exact-type filter pattern={:?} before={} after={}",
            pattern,
            before,
            after
        );
    }
}

fn search_tokens(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn should_use_symbol_fts(pattern: &str) -> bool {
    let compact = compact_identifier(pattern.trim());
    compact.len() >= 3
}

fn should_use_file_fts(pattern: &str) -> bool {
    pattern.trim().chars().count() >= 3
}

fn quoted_fts_term(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

fn build_symbol_fts_query(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }

    if is_identifier_query(pattern) {
        let compact = compact_identifier(pattern);
        if compact.len() >= 3 {
            return Some(quoted_fts_term(&compact));
        }
    }

    let terms = search_tokens(pattern)
        .into_iter()
        .filter(|token| token.chars().count() >= 3)
        .map(|token| quoted_fts_term(&token))
        .collect::<Vec<_>>();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn build_file_fts_query(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }

    let terms = search_tokens(pattern)
        .into_iter()
        .filter(|token| token.chars().count() >= 3)
        .map(|token| quoted_fts_term(&token))
        .collect::<Vec<_>>();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn candidate_values_clause(count: usize) -> String {
    (0..count)
        .map(|index| format!("(?, {index})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn search_symbol_candidate_ids(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Option<Vec<i64>>> {
    if !should_use_symbol_fts(pattern) {
        return Ok(None);
    }

    let Some(query) = build_symbol_fts_query(pattern) else {
        return Ok(None);
    };

    let candidate_limit = limit.clamp(64, 8192) as i64;
    let mut stmt = conn.prepare(
        "SELECT rowid
         FROM search_symbols_fts
         WHERE search_symbols_fts MATCH ?1
         ORDER BY bm25(search_symbols_fts), rowid
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![query, candidate_limit], |row| row.get::<_, i64>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(Some(ids))
}

fn search_file_candidate_ids(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Option<Vec<i64>>> {
    if !should_use_file_fts(pattern) {
        return Ok(None);
    }

    let Some(query) = build_file_fts_query(pattern) else {
        return Ok(None);
    };

    let candidate_limit = limit.clamp(64, 8192) as i64;
    let mut stmt = conn.prepare(
        "SELECT rowid
         FROM search_files_fts
         WHERE search_files_fts MATCH ?1
         ORDER BY bm25(search_files_fts), rowid
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![query, candidate_limit], |row| row.get::<_, i64>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(Some(ids))
}

fn find_result_identity(item: &Value) -> String {
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
    let name = item
        .get("name")
        .or_else(|| item.get("symbol_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = item
        .get("type")
        .or_else(|| item.get("symbol_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    format!("{kind}\t{path}\t{line}\t{name}")
}

fn merge_bucket_results(
    target: &mut Vec<Value>,
    seen: &mut std::collections::HashSet<String>,
    items: Vec<Value>,
    limit: usize,
) {
    for item in items {
        if target.len() >= limit {
            break;
        }
        let key = find_result_identity(&item);
        if seen.insert(key) {
            target.push(item);
        }
    }
}

fn merge_bucket_results_value(
    target: &mut Vec<Value>,
    seen: &mut std::collections::HashSet<String>,
    value: Value,
    limit: usize,
) {
    if let Some(items) = value.as_array() {
        merge_bucket_results(target, seen, items.clone(), limit);
    }
}

fn search_files_for_global(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    bucketed_file_results(conn, pattern, limit)
}

fn symbol_row_to_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "name": row.get::<_, String>(0)?,
        "type": row.get::<_, String>(1)?,
        "class_name": row.get::<_, Option<String>>(2)?,
        "path": normalize_path(&row.get::<_, String>(3)?),
        "line": row.get::<_, Option<i64>>(4)?,
        "module_name": row.get::<_, Option<String>>(5)?,
    }))
}

fn file_row_to_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "name": row.get::<_, String>(0)?,
        "type": "file",
        "path": normalize_path(&row.get::<_, String>(1)?),
        "line": 1,
        "module_name": row.get::<_, Option<String>>(2)?,
        "module_root": row.get::<_, Option<String>>(3)?.map(|p| normalize_path(&p)),
    }))
}

fn fetch_values_with_params<F>(
    conn: &Connection,
    sql: &str,
    params: Vec<SqlValue>,
    mut map_row: F,
) -> anyhow::Result<Vec<Value>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| map_row(row))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn bucketed_symbol_results(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    class_only: bool,
) -> anyhow::Result<Vec<Value>> {
    let limit = limit.clamp(1, 10_000);
    let query_text = pattern.trim();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }

    let query = query_text.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let identifier_query = is_identifier_query(query_text);
    let compact_query = if identifier_query {
        compact_identifier(query_text)
    } else {
        String::new()
    };
    let compact_prefix_query = if compact_query.is_empty() {
        String::new()
    } else {
        format!("{}%", escape_like(&compact_query))
    };
    let class_filter = if class_only { " AND is_class_like = 1" } else { "" };
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let exact_sql = if identifier_query {
        format!(
            r#"
            SELECT name, kind, owner_name, path, line_number, module_name
            FROM search_symbols
            WHERE (name_lc = ? OR compact_name = ?){class_filter}
            ORDER BY kind_rank ASC, name_lc ASC, path_lc ASC
            LIMIT ?
            "#
        )
    } else {
        format!(
            r#"
            SELECT name, kind, owner_name, path, line_number, module_name
            FROM search_symbols
            WHERE name_lc = ?{class_filter}
            ORDER BY kind_rank ASC, name_lc ASC, path_lc ASC
            LIMIT ?
            "#
        )
    };
    let mut exact_params = vec![SqlValue::Text(query.clone())];
    if identifier_query {
        exact_params.push(SqlValue::Text(compact_query.clone()));
    }
    exact_params.push(SqlValue::Integer(limit as i64));
    merge_bucket_results(
        &mut results,
        &mut seen,
        fetch_values_with_params(conn, &exact_sql, exact_params, symbol_row_to_value)?,
        limit,
    );

    if results.len() < limit {
        let remaining = limit.saturating_sub(results.len());
        let prefix_sql = if identifier_query {
            format!(
                r#"
                SELECT name, kind, owner_name, path, line_number, module_name
                FROM search_symbols
                WHERE (
                    (name_lc LIKE ? ESCAPE '\' AND name_lc <> ?)
                    OR (compact_name LIKE ? ESCAPE '\' AND compact_name <> ?)
                ){class_filter}
                ORDER BY
                    CASE
                        WHEN name_lc LIKE ? ESCAPE '\' THEN 0
                        WHEN compact_name LIKE ? ESCAPE '\' THEN 1
                        ELSE 2
                    END,
                    kind_rank ASC,
                    name_lc ASC,
                    path_lc ASC
                LIMIT ?
                "#
            )
        } else {
            format!(
                r#"
                SELECT name, kind, owner_name, path, line_number, module_name
                FROM search_symbols
                WHERE name_lc LIKE ? ESCAPE '\'
                  AND name_lc <> ?{class_filter}
                ORDER BY kind_rank ASC, name_lc ASC, path_lc ASC
                LIMIT ?
                "#
            )
        };
        let mut prefix_params = vec![
            SqlValue::Text(prefix_query.clone()),
            SqlValue::Text(query.clone()),
        ];
        if identifier_query {
            prefix_params.push(SqlValue::Text(compact_prefix_query.clone()));
            prefix_params.push(SqlValue::Text(compact_query.clone()));
            prefix_params.push(SqlValue::Text(prefix_query.clone()));
            prefix_params.push(SqlValue::Text(compact_prefix_query.clone()));
        }
        prefix_params.push(SqlValue::Integer(remaining as i64));
        merge_bucket_results(
            &mut results,
            &mut seen,
            fetch_values_with_params(conn, &prefix_sql, prefix_params, symbol_row_to_value)?,
            limit,
        );
    }

    if results.len() < limit {
        let remaining = limit.saturating_sub(results.len());
        let candidate_limit = limit.saturating_mul(12);
        if let Some(candidate_ids) = search_symbol_candidate_ids(conn, query_text, candidate_limit)? {
            if !candidate_ids.is_empty() {
                let values_clause = candidate_values_clause(candidate_ids.len());
                let fts_sql = if identifier_query {
                    format!(
                        r#"
                        WITH candidates(id, ord) AS (VALUES {values_clause})
                        SELECT s.name, s.kind, s.owner_name, s.path, s.line_number, s.module_name
                        FROM candidates c
                        JOIN search_symbols s ON s.id = c.id
                        WHERE 1 = 1{class_filter}
                        ORDER BY
                            CASE
                                WHEN s.name_lc = ? THEN 0
                                WHEN s.name_lc LIKE ? ESCAPE '\' THEN 1
                                WHEN s.compact_name = ? THEN 2
                                WHEN s.compact_name LIKE ? ESCAPE '\' THEN 3
                                ELSE 4
                            END,
                            s.kind_rank ASC,
                            c.ord ASC,
                            s.name_lc ASC,
                            s.path_lc ASC
                        LIMIT ?
                        "#
                    )
                } else {
                    format!(
                        r#"
                        WITH candidates(id, ord) AS (VALUES {values_clause})
                        SELECT s.name, s.kind, s.owner_name, s.path, s.line_number, s.module_name
                        FROM candidates c
                        JOIN search_symbols s ON s.id = c.id
                        WHERE 1 = 1{class_filter}
                        ORDER BY
                            CASE
                                WHEN s.name_lc = ? THEN 0
                                WHEN s.name_lc LIKE ? ESCAPE '\' THEN 1
                                WHEN s.owner_name_lc = ? THEN 2
                                WHEN s.owner_name_lc LIKE ? ESCAPE '\' THEN 3
                                WHEN s.module_name_lc = ? THEN 4
                                WHEN s.module_name_lc LIKE ? ESCAPE '\' THEN 5
                                ELSE 6
                            END,
                            s.kind_rank ASC,
                            c.ord ASC,
                            s.name_lc ASC,
                            s.path_lc ASC
                        LIMIT ?
                        "#
                    )
                };

                let mut fts_params = candidate_ids
                    .into_iter()
                    .map(SqlValue::Integer)
                    .collect::<Vec<_>>();
                fts_params.push(SqlValue::Text(query.clone()));
                fts_params.push(SqlValue::Text(prefix_query.clone()));
                if identifier_query {
                    fts_params.push(SqlValue::Text(compact_query));
                    fts_params.push(SqlValue::Text(compact_prefix_query));
                } else {
                    fts_params.push(SqlValue::Text(query.clone()));
                    fts_params.push(SqlValue::Text(prefix_query.clone()));
                    fts_params.push(SqlValue::Text(query));
                    fts_params.push(SqlValue::Text(prefix_query));
                }
                fts_params.push(SqlValue::Integer(remaining as i64));
                merge_bucket_results(
                    &mut results,
                    &mut seen,
                    fetch_values_with_params(conn, &fts_sql, fts_params, symbol_row_to_value)?,
                    limit,
                );
            }
        }
    }

    Ok(results)
}

fn bucketed_file_results(conn: &Connection, pattern: &str, limit: usize) -> anyhow::Result<Vec<Value>> {
    let limit = limit.clamp(1, 10_000);
    let query_text = pattern.trim();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }

    let query = query_text.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let exact_sql = format!(
        r#"
        {}
        SELECT sf.basename, sf.path, sf.module_name, rd.full_path AS module_root
        FROM search_files sf
        LEFT JOIN modules m ON sf.module_id = m.id
        LEFT JOIN dir_paths rd ON m.root_directory_id = rd.id
        WHERE (sf.basename_lc = ? OR sf.path_lc = ?)
          AND lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
        ORDER BY sf.basename_lc ASC, sf.path_lc ASC
        LIMIT ?
        "#,
        PATH_CTE
    );
    merge_bucket_results(
        &mut results,
        &mut seen,
        fetch_values_with_params(
            conn,
            &exact_sql,
            vec![
                SqlValue::Text(query.clone()),
                SqlValue::Text(query.clone()),
                SqlValue::Integer(limit as i64),
            ],
            file_row_to_value,
        )?,
        limit,
    );

    if results.len() < limit {
        let remaining = limit.saturating_sub(results.len());
        let prefix_sql = format!(
            r#"
            {}
            SELECT sf.basename, sf.path, sf.module_name, rd.full_path AS module_root
            FROM search_files sf
            LEFT JOIN modules m ON sf.module_id = m.id
            LEFT JOIN dir_paths rd ON m.root_directory_id = rd.id
            WHERE (
                (sf.basename_lc LIKE ? ESCAPE '\' AND sf.basename_lc <> ?)
                OR (sf.path_lc LIKE ? ESCAPE '\' AND sf.path_lc <> ?)
            )
              AND lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
            ORDER BY
                CASE
                    WHEN sf.basename_lc LIKE ? ESCAPE '\' THEN 0
                    WHEN sf.path_lc LIKE ? ESCAPE '\' THEN 1
                    ELSE 2
                END,
                sf.basename_lc ASC,
                sf.path_lc ASC
            LIMIT ?
            "#,
            PATH_CTE
        );
        merge_bucket_results(
            &mut results,
            &mut seen,
            fetch_values_with_params(
                conn,
                &prefix_sql,
                vec![
                    SqlValue::Text(prefix_query.clone()),
                    SqlValue::Text(query.clone()),
                    SqlValue::Text(prefix_query.clone()),
                    SqlValue::Text(query.clone()),
                    SqlValue::Text(prefix_query.clone()),
                    SqlValue::Text(prefix_query.clone()),
                    SqlValue::Integer(remaining as i64),
                ],
                file_row_to_value,
            )?,
            limit,
        );
    }

    if results.len() < limit {
        let remaining = limit.saturating_sub(results.len());
        let candidate_limit = limit.saturating_mul(12);
        if let Some(candidate_ids) = search_file_candidate_ids(conn, query_text, candidate_limit)? {
            if !candidate_ids.is_empty() {
                let values_clause = candidate_values_clause(candidate_ids.len());
                let fts_sql = format!(
                    r#"
                    {}
                    WITH candidates(id, ord) AS (VALUES {values_clause})
                    SELECT sf.basename, sf.path, sf.module_name, rd.full_path AS module_root
                    FROM candidates c
                    JOIN search_files sf ON sf.file_id = c.id
                    LEFT JOIN modules m ON sf.module_id = m.id
                    LEFT JOIN dir_paths rd ON m.root_directory_id = rd.id
                    WHERE lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
                    ORDER BY
                        CASE
                            WHEN sf.basename_lc = ? THEN 0
                            WHEN sf.basename_lc LIKE ? ESCAPE '\' THEN 1
                            WHEN sf.path_lc = ? THEN 2
                            WHEN sf.path_lc LIKE ? ESCAPE '\' THEN 3
                            ELSE 4
                        END,
                        c.ord ASC,
                        sf.basename_lc ASC,
                        sf.path_lc ASC
                    LIMIT ?
                    "#,
                    PATH_CTE
                );
                let mut fts_params = candidate_ids
                    .into_iter()
                    .map(SqlValue::Integer)
                    .collect::<Vec<_>>();
                fts_params.push(SqlValue::Text(query.clone()));
                fts_params.push(SqlValue::Text(prefix_query.clone()));
                fts_params.push(SqlValue::Text(query));
                fts_params.push(SqlValue::Text(prefix_query));
                fts_params.push(SqlValue::Integer(remaining as i64));
                merge_bucket_results(
                    &mut results,
                    &mut seen,
                    fetch_values_with_params(conn, &fts_sql, fts_params, file_row_to_value)?,
                    limit,
                );
            }
        }
    }

    Ok(results)
}

fn search_symbols_fuzzy_fallback(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 1000) as i64;
    let tokens = search_tokens(pattern);
    if tokens.is_empty() {
        return Ok(json!([]));
    }

    let allow_owner_match = !is_explicit_type_query(pattern.trim());

    let token_filter = tokens
        .iter()
        .map(|_| {
            let compact_like = "(compact_name LIKE ? ESCAPE '\\' OR compact_name LIKE ? ESCAPE '\\')";
            if allow_owner_match {
                format!("({compact_like} OR owner_name_lc LIKE ? ESCAPE '\\')")
            } else {
                compact_like.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    let sql = format!(
        r#"
        SELECT
            name,
            kind,
            owner_name,
            path,
            line_number,
            module_name
        FROM search_symbols
        WHERE {}
        ORDER BY
            kind_rank ASC,
            length(compact_name) ASC,
            name_lc ASC,
            path_lc ASC
        LIMIT ?
        "#,
        token_filter
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut params = Vec::new();
    for token in &tokens {
        let compact_token = compact_identifier(token);
        let like = format!("%{}%", escape_like(&compact_token));
        let prefix = compact_token.chars().take(5).collect::<String>();
        let prefix_like = format!("{}%", escape_like(&prefix));
        params.push(SqlValue::Text(like.clone()));
        params.push(SqlValue::Text(prefix_like));
        if allow_owner_match {
            params.push(SqlValue::Text(format!("%{}%", escape_like(token))));
        }
    }
    params.push(SqlValue::Integer(limit));

    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "type": row.get::<_, String>(1)?,
            "class_name": row.get::<_, Option<String>>(2)?,
            "path": normalize_path(&row.get::<_, String>(3)?),
            "line": row.get::<_, Option<i64>>(4)?,
            "module_name": row.get::<_, Option<String>>(5)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

fn search_files_fuzzy_fallback(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 1000) as i64;
    let tokens = search_tokens(pattern);
    if tokens.is_empty() {
        return Ok(json!([]));
    }

    let token_filter = tokens
        .iter()
        .map(|_| {
            "(basename_lc LIKE ? ESCAPE '\\' OR path_lc LIKE ? ESCAPE '\\')".to_string()
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    let sql = format!(
        r#"
        {}
        SELECT
            sf.basename AS filename,
            sf.path,
            sf.module_name,
            rd.full_path AS module_root
        FROM search_files sf
        LEFT JOIN modules m ON sf.module_id = m.id
        LEFT JOIN dir_paths rd ON m.root_directory_id = rd.id
        WHERE {}
          AND lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
        ORDER BY
            length(sf.basename_lc) ASC,
            sf.basename_lc ASC,
            sf.path_lc ASC
        LIMIT ?
        "#,
        PATH_CTE,
        token_filter
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut params = Vec::new();
    for token in &tokens {
        let like = format!("%{}%", escape_like(token));
        params.push(SqlValue::Text(like.clone()));
        params.push(SqlValue::Text(like));
    }
    params.push(SqlValue::Integer(limit));

    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "type": "file",
            "path": normalize_path(&row.get::<_, String>(1)?),
            "line": 1,
            "module_name": row.get::<_, Option<String>>(2)?,
            "module_root": row.get::<_, Option<String>>(3)?.map(|p| normalize_path(&p)),
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

fn search_text_for_global(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    ensure_search_projections(conn)?;
    search_code_text(conn, pattern, limit, 0)
}

pub fn search_code_text(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    ensure_search_projections(conn)?;
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Ok(json!([]));
    }

    let matches = text::search_matching_lines(conn, pattern, limit.clamp(1, 500), offset.min(1_000_000))?;
    let mut results = Vec::with_capacity(matches.len());
    for item in matches {
        results.push(json!({
            "name": pattern,
            "type": "text",
            "path": normalize_path(&item.path),
            "line": item.line_number,
            "text": item.line_text,
        }));
    }

    Ok(json!(results))
}

/// Get all indexed C++/Unreal structs.
/// 获取所有已经索引到的 C++/Unreal struct。
pub fn get_structs(conn: &Connection) -> anyhow::Result<Value> {
    let sql = format!(
        r#"
        {}
        SELECT
            sc.text AS name,
            sb.text AS base_class,
            c.symbol_type,
            dp.full_path || '/' || sn.text AS path,
            sm.text AS module_name,
            c.line_number,
            c.end_line_number
        FROM classes c
        JOIN strings sc ON c.name_id = sc.id
        LEFT JOIN strings sb ON c.base_class_id = sb.id
        JOIN files f ON c.file_id = f.id
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        LEFT JOIN modules m ON f.module_id = m.id
        LEFT JOIN strings sm ON m.name_id = sm.id
        WHERE c.symbol_type IN ('struct', 'USTRUCT')
          AND sc.text NOT LIKE '(%'
        ORDER BY sc.text ASC
        "#,
        PATH_CTE
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "base_class": row.get::<_, Option<String>>(1)?,
            "type": row.get::<_, String>(2)?,
            "path": normalize_path(&row.get::<_, String>(3)?),
            "module_name": row.get::<_, Option<String>>(4)?,
            "line": row.get::<_, Option<i64>>(5)?,
            "end_line": row.get::<_, Option<i64>>(6)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

/// Get all indexed classes, structs, or enums by symbol type.
/// 按 symbol_type 获取 class、struct、enum 等类型符号。
pub fn get_symbols_by_type(
    conn: &Connection,
    symbol_type: &str,
    limit: Option<usize>,
) -> anyhow::Result<Value> {
    let symbol_type = symbol_type.trim();

    if symbol_type.is_empty() {
        return Ok(json!([]));
    }

    let limit = limit.unwrap_or(1000).clamp(1, 5000) as i64;

    let sql = format!(
        r#"
        {}
        SELECT
            sc.text AS name,
            sb.text AS base_class,
            c.symbol_type,
            dp.full_path || '/' || sn.text AS path,
            sm.text AS module_name,
            c.line_number,
            c.end_line_number
        FROM classes c
        JOIN strings sc ON c.name_id = sc.id
        LEFT JOIN strings sb ON c.base_class_id = sb.id
        JOIN files f ON c.file_id = f.id
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        LEFT JOIN modules m ON f.module_id = m.id
        LEFT JOIN strings sm ON m.name_id = sm.id
        WHERE c.symbol_type = ?
          AND sc.text NOT LIKE '(%'
        ORDER BY sc.text ASC
        LIMIT ?
        "#,
        PATH_CTE
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![symbol_type, limit], |row| {
        Ok(json!({
            "name": row.get::<_, String>(0)?,
            "base_class": row.get::<_, Option<String>>(1)?,
            "type": row.get::<_, String>(2)?,
            "path": normalize_path(&row.get::<_, String>(3)?),
            "module_name": row.get::<_, Option<String>>(4)?,
            "line": row.get::<_, Option<i64>>(5)?,
            "end_line": row.get::<_, Option<i64>>(6)?,
        }))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
}

/// Normalize Windows paths to slash-separated paths.
/// 把 Windows 反斜杠路径统一成斜杠路径。
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").replace("//", "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn insert_string(conn: &Connection, text: &str) -> i64 {
        conn.execute("INSERT OR IGNORE INTO strings (text) VALUES (?)", [text])
            .unwrap();
        conn.query_row("SELECT id FROM strings WHERE text = ?", [text], |row| row.get(0))
            .unwrap()
    }

    fn get_or_create_dir(conn: &Connection, parent_id: Option<i64>, name_id: i64) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO directories (parent_id, name_id) VALUES (?, ?)",
            params![parent_id, name_id],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM directories WHERE parent_id IS ?1 AND name_id = ?2",
            params![parent_id, name_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn insert_project_header_file(conn: &Connection, module: &str, filename: &str) -> i64 {
        let drive = insert_string(conn, "C:");
        let project_name = insert_string(conn, "Project");
        let source_name = insert_string(conn, "Source");
        let module_name = insert_string(conn, module);
        let public_name = insert_string(conn, "Public");
        let filename_id = insert_string(conn, filename);

        let c_dir = get_or_create_dir(conn, None, drive);
        let project_dir = get_or_create_dir(conn, Some(c_dir), project_name);
        let source_dir = get_or_create_dir(conn, Some(project_dir), source_name);
        let module_dir = get_or_create_dir(conn, Some(source_dir), module_name);
        let public_dir = get_or_create_dir(conn, Some(module_dir), public_name);

        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?, ?, 'h', 1)",
            params![public_dir, filename_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_class_symbol(conn: &Connection, file_id: i64, name: &str) -> i64 {
        let name_id = insert_string(conn, name);
        conn.execute(
            "INSERT INTO classes (name_id, file_id, line_number, end_line_number, symbol_type) VALUES (?, ?, 1, 1, 'class')",
            params![name_id, file_id],
        )
        .unwrap();
        let class_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols_fts (name, type, class_name, rowid_ref) VALUES (?, 'class', ?, ?)",
            params![name, name, class_id],
        )
        .unwrap();
        class_id
    }

    fn insert_property_symbol(conn: &Connection, class_id: i64, file_id: i64, class_name: &str, name: &str) -> i64 {
        let name_id = insert_string(conn, name);
        let type_id = insert_string(conn, "property");
        conn.execute(
            "INSERT INTO members (class_id, name_id, type_id, flags, access, detail, return_type_id, is_static, line_number, file_id)
             VALUES (?, ?, ?, 'UPROPERTY', 'private', NULL, NULL, 0, 2, ?)",
            params![class_id, name_id, type_id, file_id],
        )
        .unwrap();
        let member_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols_fts (name, type, class_name, rowid_ref) VALUES (?, 'property', ?, ?)",
            params![name, class_name, member_id],
        )
        .unwrap();
        member_id
    }

    #[test]
    fn fast_find_falls_back_to_subsequence_matches_for_symbols() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "RangedWeapon.h");
        insert_class_symbol(&conn, file_id, "RangedWeapon");

        let items = fast_find(&conn, "rangeweapon", 20, 0).unwrap();
        let items = items.as_array().unwrap();
        assert!(items.iter().any(|item| item["name"] == "RangedWeapon"));
    }

    #[test]
    fn fast_find_keeps_strong_matches_ahead_of_subsequence_fallback() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let exact_file_id = insert_project_header_file(&conn, "Game", "RangeWeapon.h");
        insert_class_symbol(&conn, exact_file_id, "RangeWeapon");

        let fuzzy_file_id = insert_project_header_file(&conn, "Game", "RangedWeapon.h");
        insert_class_symbol(&conn, fuzzy_file_id, "RangedWeapon");

        let items = fast_find(&conn, "rangeweapon", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        let exact_index = items
            .iter()
            .position(|item| item["name"] == "RangeWeapon")
            .unwrap();
        let fuzzy_index = items
            .iter()
            .position(|item| item["name"] == "RangedWeapon")
            .unwrap();

        assert!(exact_index < fuzzy_index);
    }

    #[test]
    fn fast_find_suppresses_broad_class_contains_when_exact_type_exists() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let exact_file_id = insert_project_header_file(&conn, "Game", "GameplayAbility.h");
        let exact_class_id = insert_class_symbol(&conn, exact_file_id, "UGameplayAbility");
        insert_property_symbol(
            &conn,
            exact_class_id,
            exact_file_id,
            "UGameplayAbility",
            "AbilityTags",
        );

        let broad_file_id = insert_project_header_file(&conn, "Game", "GameplayAbilityStatics.h");
        insert_class_symbol(&conn, broad_file_id, "UGameplayAbilityStatics");

        let items = fast_find(&conn, "UGameplayAbility", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "UGameplayAbility"));
        assert!(!items.iter().any(|item| item["name"] == "UGameplayAbilityStatics"));
    }

    #[test]
    fn fast_find_matches_unreal_type_names_without_underscores() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "GameplayCueNotify_Actor.h");
        insert_class_symbol(&conn, file_id, "AGameplayCueNotify_Actor");

        let items = fast_find(&conn, "GameplayCueNotifyActor", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "AGameplayCueNotify_Actor"));
    }

    #[test]
    fn fast_find_treats_lowercase_unreal_type_query_as_type_like() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let exact_file_id = insert_project_header_file(&conn, "Game", "GameplayAbility.h");
        let exact_class_id = insert_class_symbol(&conn, exact_file_id, "UGameplayAbility");
        insert_property_symbol(
            &conn,
            exact_class_id,
            exact_file_id,
            "UGameplayAbility",
            "AbilityTags",
        );

        let broad_file_id = insert_project_header_file(&conn, "Game", "GameplayAbilityStatics.h");
        insert_class_symbol(&conn, broad_file_id, "UGameplayAbilityStatics");

        let items = fast_find(&conn, "ugameplayability", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "UGameplayAbility"));
        assert!(!items.iter().any(|item| item["name"] == "AbilityTags"));
    }

    #[test]
    fn fast_find_identifier_query_skips_file_bucket() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "GameplayAbility.h");
        insert_class_symbol(&conn, file_id, "UGameplayAbility");

        let items = fast_find(&conn, "UGameplayAbility", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().all(|item| item["type"] != "file"));
    }

    #[test]
    fn hot_index_fast_find_returns_exact_and_prefix_matches() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "GameplayAbility.h");
        insert_class_symbol(&conn, file_id, "UGameplayAbility");

        let hot_index = build_search_hot_index(&conn).unwrap();
        let items = fast_find_with_hot_index(&conn, Some(&hot_index), "UGameplayAbility", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "UGameplayAbility"));
        assert!(items.iter().all(|item| item["type"] != "file"));
    }

    #[test]
    fn hot_index_matches_unreal_type_without_underscores() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "GameplayCueNotify_Actor.h");
        insert_class_symbol(&conn, file_id, "AGameplayCueNotify_Actor");

        let hot_index = build_search_hot_index(&conn).unwrap();
        let items =
            fast_find_with_hot_index(&conn, Some(&hot_index), "GameplayCueNotifyActor", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items
            .iter()
            .any(|item| item["name"] == "AGameplayCueNotify_Actor"));
    }

    #[test]
    fn hot_index_search_symbols_can_match_owner_name() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Game", "GameplayAbility.h");
        let class_id = insert_class_symbol(&conn, file_id, "UGameplayAbility");
        insert_property_symbol(&conn, class_id, file_id, "UGameplayAbility", "AbilityTags");

        let hot_index = build_search_hot_index(&conn).unwrap();
        let items =
            search_symbols_with_hot_index(&conn, Some(&hot_index), "UGameplayAbility", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "AbilityTags"));
    }

    #[test]
    fn hot_index_search_symbols_can_match_module_name() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        let file_id = insert_project_header_file(&conn, "Gameplay", "GameplayAbility.h");
        insert_class_symbol(&conn, file_id, "UGameplayAbility");

        let hot_index = build_search_hot_index(&conn).unwrap();
        let items = search_symbols_with_hot_index(&conn, Some(&hot_index), "gameplay", 20, 0).unwrap();
        let items = items.as_array().unwrap();

        assert!(items.iter().any(|item| item["name"] == "UGameplayAbility"));
    }

    #[test]
    fn search_code_text_reads_from_dedicated_text_db() {
        let base = std::env::temp_dir().join(format!(
            "ucore-text-search-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();

        let source_path = base.join("GameplayAbility.cpp");
        fs::write(
            &source_path,
            "void Test()\n{\n    UGameplayAbility::ActivateAbility();\n}\n",
        )
        .unwrap();

        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        crate::db::init_db(&conn).unwrap();
        crate::db::text::sync_text_files(
            db_path.to_string_lossy().as_ref(),
            &[crate::db::text::TextIndexFile {
                path: source_path.to_string_lossy().to_string(),
                extension: "cpp".to_string(),
                mtime: 0,
            }],
            None,
        )
        .unwrap();

        let items = search_code_text(&conn, "GameplayAbility::ActivateAbility", 20, 0).unwrap();
        let items = items.as_array().unwrap();
        assert!(!items.is_empty());
        assert!(items
            .iter()
            .any(|item| item["path"] == source_path.to_string_lossy().replace('\\', "/")));

        let _ = fs::remove_file(crate::db::text::derived_text_db_path(
            db_path.to_string_lossy().as_ref(),
        ));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(&source_path);
        let _ = fs::remove_dir(&base);
    }
}
