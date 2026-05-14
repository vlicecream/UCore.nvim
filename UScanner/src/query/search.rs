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
use std::sync::OnceLock;
use tracing::info;

use crate::db::project_path::PATH_CTE;

const FIND_FUZZY_FALLBACK_LIMIT: usize = 256;
static FAST_FIND_LOG_ENABLED: OnceLock<bool> = OnceLock::new();

fn fast_find_log_enabled() -> bool {
    *FAST_FIND_LOG_ENABLED.get_or_init(|| {
        std::env::var("UCORE_FAST_FIND_LOG")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(false)
    })
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
    let pattern = pattern.trim();

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let limit = limit.clamp(1, 10_000) as i64;
    let offset = offset.min(1_000_000) as i64;
    let query = pattern.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let contains_query = format!("%{}%", escape_like(&query));
    let tokens = search_tokens(pattern);
    let allow_owner_match = !is_identifier_query(pattern);
    let searchable = if allow_owner_match {
        "lower(COALESCE(name, '') || ' ' || COALESCE(kind, '') || ' ' || COALESCE(owner_name, '') || ' ' || COALESCE(module_name, '') || ' ' || COALESCE(path, ''))"
    } else {
        "lower(COALESCE(name, '') || ' ' || COALESCE(kind, '') || ' ' || COALESCE(module_name, '') || ' ' || COALESCE(path, ''))"
    };
    let token_filter = tokens
        .iter()
        .map(|_| format!("{searchable} LIKE ? ESCAPE '\\'"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let where_clause = if token_filter.is_empty() {
        "1 = 1".to_string()
    } else {
        token_filter
    };

    let owner_rank_sql = if allow_owner_match {
        "WHEN owner_name_lc LIKE ? ESCAPE '\\' THEN 3"
    } else {
        ""
    };
    let module_rank_sql = if allow_owner_match {
        "WHEN module_name_lc LIKE ? ESCAPE '\\' THEN 4"
    } else {
        "WHEN module_name_lc LIKE ? ESCAPE '\\' THEN 3"
    };
    let path_rank_sql = if allow_owner_match {
        "WHEN path_lc LIKE ? ESCAPE '\\' THEN 5"
    } else {
        "WHEN path_lc LIKE ? ESCAPE '\\' THEN 4"
    };

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
            CASE
                WHEN name_lc = ? THEN 0
                WHEN name_lc LIKE ? ESCAPE '\' THEN 1
                WHEN name_lc LIKE ? ESCAPE '\' THEN 2
                {}
                {}
                {}
                ELSE 9
            END,
            kind_rank ASC,
            name_lc ASC,
            path_lc ASC
        LIMIT ? OFFSET ?
        "#,
        where_clause,
        owner_rank_sql,
        module_rank_sql,
        path_rank_sql
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut params = Vec::new();
    for token in &tokens {
        params.push(SqlValue::Text(format!("%{}%", escape_like(token))));
    }
    params.push(SqlValue::Text(query));
    params.push(SqlValue::Text(prefix_query));
    params.push(SqlValue::Text(contains_query.clone()));
    if allow_owner_match {
        params.push(SqlValue::Text(contains_query.clone()));
    }
    params.push(SqlValue::Text(contains_query.clone()));
    params.push(SqlValue::Text(contains_query));
    params.push(SqlValue::Integer(limit));
    params.push(SqlValue::Integer(offset));

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

    extend_json_array(&mut results, search_symbols(conn, pattern, target.max(limit), 0)?);
    extend_json_array(&mut results, search_files_for_global(conn, pattern, target.max(limit))?);

    if results.len() < target {
        extend_json_array(
            &mut results,
            search_symbols_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
        );
        extend_json_array(
            &mut results,
            search_files_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
        );
    }

    if results.len() < target {
        extend_json_array(&mut results, search_text_for_global(conn, pattern, target)?);
    }

    dedupe_find_results(&mut results);

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
    let pattern = pattern.trim();
    let limit = limit.clamp(1, 500);
    let offset = offset.min(1_000_000);

    if pattern.is_empty() {
        return list_symbols(conn, limit, offset);
    }

    let target = offset.saturating_add(limit);
    let mut results = Vec::new();
    let identifier_query = is_identifier_query(pattern);

    extend_json_array(&mut results, fast_find_symbols(conn, pattern, target.max(limit))?);
    if !identifier_query {
        extend_json_array(&mut results, search_files_for_global(conn, pattern, target.max(limit))?);
    }

    let should_run_fallback = results.len() < target && !has_strong_explicit_type_match(&results, pattern);
    if should_run_fallback {
        extend_json_array(
            &mut results,
            search_symbols_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
        );
        if !identifier_query {
            extend_json_array(
                &mut results,
                search_files_fuzzy_fallback(conn, pattern, FIND_FUZZY_FALLBACK_LIMIT)?,
            );
        }
    }

    dedupe_find_results(&mut results);
    suppress_broad_class_contains_when_exact_type_exists(&mut results, pattern);

    let page = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    Ok(json!(page))
}

fn fast_find_symbols(conn: &Connection, pattern: &str, limit: usize) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 500) as i64;
    let query_text = pattern.trim();
    let query = query_text.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let contains_query = format!("%{}%", escape_like(&query));
    let identifier_query = is_identifier_query(query_text);
    let allow_owner_match = !identifier_query;
    let allow_compact_name_match = identifier_query;
    let compact_query = compact_identifier(query_text);
    let compact_prefix_query = format!("{}%", escape_like(&compact_query));
    let compact_contains_query = format!("%{}%", escape_like(&compact_query));
    let owner_rank_sql = if allow_owner_match {
        "WHEN owner_name_lc LIKE ? ESCAPE '\\' THEN 3"
    } else {
        ""
    };
    let owner_where_sql = if allow_owner_match {
        " OR owner_name_lc LIKE ? ESCAPE '\\'"
    } else {
        ""
    };
    let compact_rank_sql = if allow_compact_name_match {
        r#"
                    WHEN compact_name = ? THEN 3
                    WHEN compact_name LIKE ? ESCAPE '\' THEN 4
                    WHEN compact_name LIKE ? ESCAPE '\' THEN 5
        "#
    } else {
        ""
    };
    let compact_where_sql = if allow_compact_name_match {
        " OR compact_name LIKE ? ESCAPE '\\'"
    } else {
        ""
    };

    let sql = format!(
        r#"
        WITH matched AS (
            SELECT
                name,
                kind,
                owner_name,
                path,
                line_number,
                module_name,
                CASE
                    WHEN name_lc = ? THEN 0
                    WHEN name_lc LIKE ? ESCAPE '\' THEN 1
                    WHEN name_lc LIKE ? ESCAPE '\' THEN 2
                    {}
                    {}
                    ELSE 9
                END AS rank
            FROM search_symbols
            WHERE name_lc LIKE ? ESCAPE '\'
               {}
               {}
            ORDER BY rank, kind_rank ASC, name_lc ASC, path_lc ASC
            LIMIT ?
        )
        SELECT
            name,
            kind,
            owner_name,
            path,
            line_number,
            module_name
        FROM matched
        ORDER BY rank, lower(name) ASC, path ASC
        "#,
        compact_rank_sql,
        owner_rank_sql,
        compact_where_sql,
        owner_where_sql
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut params = vec![
        SqlValue::Text(query),
        SqlValue::Text(prefix_query),
        SqlValue::Text(contains_query.clone()),
    ];
    if allow_compact_name_match {
        params.push(SqlValue::Text(compact_query.clone()));
        params.push(SqlValue::Text(compact_prefix_query));
        params.push(SqlValue::Text(compact_contains_query.clone()));
    }
    if allow_owner_match {
        params.push(SqlValue::Text(contains_query.clone()));
    }
    params.push(SqlValue::Text(contains_query.clone()));
    if allow_compact_name_match {
        params.push(SqlValue::Text(compact_contains_query));
    }
    if allow_owner_match {
        params.push(SqlValue::Text(contains_query));
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

fn list_class_symbols(conn: &Connection, limit: usize, offset: usize) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 10_000) as i64;
    let offset = offset.min(1_000_000) as i64;

    let sql = format!(
        r#"
        {}
        SELECT
            sfts.name,
            sfts.type,
            sfts.class_name,
            dp.full_path || '/' || sn.text AS path,
            COALESCE(c.line_number, mem.line_number),
            sm.text AS module_name
        FROM symbols_fts sfts
        LEFT JOIN classes c
            ON c.id = sfts.rowid_ref
           AND {}
        LEFT JOIN members mem
            ON mem.id = sfts.rowid_ref
           AND NOT ({})
        JOIN files f ON f.id = COALESCE(c.file_id, mem.file_id)
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        LEFT JOIN modules m ON f.module_id = m.id
        LEFT JOIN strings sm ON m.name_id = sm.id
        WHERE {} AND sfts.name NOT LIKE '(%'
        ORDER BY lower(sfts.name) ASC
        LIMIT ? OFFSET ?
        "#,
        PATH_CTE,
        class_symbol_predicate("sfts"),
        class_symbol_predicate("sfts"),
        class_symbol_predicate("sfts")
    );

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
    let pattern = pattern.trim();

    if pattern.is_empty() {
        return list_class_symbols(conn, limit, offset);
    }

    let limit = limit.clamp(1, 10_000) as i64;
    let offset = offset.min(1_000_000) as i64;
    let query = pattern.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let contains_query = format!("%{}%", escape_like(&query));
    let tokens = search_tokens(pattern);
    let searchable = searchable_sql();
    let token_filter = tokens
        .iter()
        .map(|_| format!("{searchable} LIKE ? ESCAPE '\\'"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let where_clause = if token_filter.is_empty() {
        "1 = 1".to_string()
    } else {
        token_filter
    };

    let sql = format!(
        r#"
        {}
        SELECT
            sfts.name,
            sfts.type,
            sfts.class_name,
            dp.full_path || '/' || sn.text AS path,
            COALESCE(c.line_number, mem.line_number),
            sm.text AS module_name
        FROM symbols_fts sfts
        LEFT JOIN classes c
            ON c.id = sfts.rowid_ref
           AND {}
        LEFT JOIN members mem
            ON mem.id = sfts.rowid_ref
           AND NOT ({})
        JOIN files f ON f.id = COALESCE(c.file_id, mem.file_id)
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        LEFT JOIN modules m ON f.module_id = m.id
        LEFT JOIN strings sm ON m.name_id = sm.id
        WHERE ({}) AND ({})
        ORDER BY
            CASE
                WHEN lower(sfts.name) = ? THEN 0
                WHEN lower(sfts.name) LIKE ? ESCAPE '\' THEN 1
                WHEN lower(sfts.name) LIKE ? ESCAPE '\' THEN 2
                WHEN lower(COALESCE(sfts.class_name, '')) LIKE ? ESCAPE '\' THEN 3
                WHEN lower(COALESCE(sm.text, '')) LIKE ? ESCAPE '\' THEN 4
                WHEN lower(dp.full_path || '/' || sn.text) LIKE ? ESCAPE '\' THEN 5
                ELSE 9
            END,
            lower(sfts.name) ASC,
            path ASC
        LIMIT ? OFFSET ?
        "#,
        PATH_CTE,
        class_symbol_predicate("sfts"),
        class_symbol_predicate("sfts"),
        where_clause,
        class_symbol_predicate("sfts")
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut params = Vec::new();
    for token in &tokens {
        params.push(SqlValue::Text(format!("%{}%", escape_like(token))));
    }
    params.push(SqlValue::Text(query));
    params.push(SqlValue::Text(prefix_query));
    params.push(SqlValue::Text(contains_query.clone()));
    params.push(SqlValue::Text(contains_query.clone()));
    params.push(SqlValue::Text(contains_query.clone()));
    params.push(SqlValue::Text(contains_query));
    params.push(SqlValue::Integer(limit));
    params.push(SqlValue::Integer(offset));

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

fn class_symbol_predicate(alias: &str) -> String {
    format!(
        "COALESCE({alias}.type, '') IN ('class', 'struct', 'enum', 'UCLASS', 'USTRUCT', 'UENUM', 'UINTERFACE')"
    )
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

fn searchable_sql() -> &'static str {
    "lower(
        COALESCE(sfts.name, '') || ' ' ||
        COALESCE(sfts.type, '') || ' ' ||
        COALESCE(sfts.class_name, '') || ' ' ||
        COALESCE(sm.text, '') || ' ' ||
        COALESCE(dp.full_path, '') || '/' || COALESCE(sn.text, '')
    )"
}

fn search_tokens(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn subsequence_like_pattern(input: &str) -> String {
    let mut out = String::from("%");
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            _ => out.push(ch),
        }
        out.push('%');
    }
    out
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn extend_json_array(target: &mut Vec<Value>, value: Value) {
    if let Some(values) = value.as_array() {
        target.extend(values.iter().cloned());
    }
}

fn dedupe_find_results(results: &mut Vec<Value>) {
    let mut seen = std::collections::HashSet::new();
    results.retain(|item| {
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

        seen.insert(format!("{kind}\t{path}\t{line}\t{name}"))
    });
}

fn search_files_for_global(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    let limit = limit.clamp(1, 500) as i64;
    let query = pattern.to_ascii_lowercase();
    let prefix_query = format!("{}%", escape_like(&query));
    let contains_query = format!("%{}%", escape_like(&query));

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
        WHERE (sf.basename_lc LIKE ? ESCAPE '\'
           OR sf.path_lc LIKE ? ESCAPE '\')
          AND lower(COALESCE(sf.ext, '')) NOT IN ('uasset', 'umap')
        ORDER BY
            CASE
                WHEN sf.basename_lc = ? THEN 0
                WHEN sf.basename_lc LIKE ? ESCAPE '\' THEN 1
                WHEN sf.basename_lc LIKE ? ESCAPE '\' THEN 2
                WHEN sf.path_lc LIKE ? ESCAPE '\' THEN 3
                ELSE 9
            END,
            sf.basename_lc ASC,
            sf.path_lc ASC
        LIMIT ?
        "#,
        PATH_CTE
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            contains_query,
            contains_query,
            query,
            prefix_query,
            contains_query,
            contains_query,
            limit
        ],
        |row| {
            Ok(json!({
                "name": row.get::<_, String>(0)?,
                "type": "file",
                "path": normalize_path(&row.get::<_, String>(1)?),
                "line": 1,
                "module_name": row.get::<_, Option<String>>(2)?,
                "module_root": row.get::<_, Option<String>>(3)?.map(|p| normalize_path(&p)),
            }))
        },
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(json!(results))
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
            if allow_owner_match {
                "(compact_name LIKE ? ESCAPE '\\' OR owner_name_lc LIKE ? ESCAPE '\\')".to_string()
            } else {
                "compact_name LIKE ? ESCAPE '\\'".to_string()
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
        let like = subsequence_like_pattern(&compact_identifier(token));
        params.push(SqlValue::Text(like.clone()));
        if allow_owner_match {
            params.push(SqlValue::Text(subsequence_like_pattern(token)));
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
        let like = subsequence_like_pattern(token);
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
    search_code_text(conn, pattern, limit, 0)
}

pub fn search_code_text(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value> {
    let needle = pattern.to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(json!([]));
    }

    let limit = limit.clamp(1, 500) as i64;
    let offset = offset.min(1_000_000) as i64;
    let prefix_query = format!("{}%", escape_like(&needle));
    let contains_query = format!("%{}%", escape_like(&needle));
    let sql = r#"
        SELECT
            path,
            line_number,
            line_text
        FROM search_text_lines
        WHERE instr(line_text_lc, ?) > 0
        ORDER BY
            CASE
                WHEN line_text_lc = ? THEN 0
                WHEN line_text_lc LIKE ? ESCAPE '\' THEN 1
                WHEN line_text_lc LIKE ? ESCAPE '\' THEN 2
                ELSE 9
            END,
            path_lc ASC,
            line_number ASC
        LIMIT ? OFFSET ?
        "#;

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(
        params![needle, needle, prefix_query, contains_query, limit, offset],
        |row| {
            Ok(json!({
                "name": pattern,
                "type": "text",
                "path": normalize_path(&row.get::<_, String>(0)?),
                "line": row.get::<_, i64>(1)?,
                "text": row.get::<_, String>(2)?,
            }))
        },
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
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
}
