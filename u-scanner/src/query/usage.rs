use anyhow::Result;
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::BufRead;

use crate::db::path::PATH_CTE;

const MAX_RESULTS: usize = 300;
const MAX_FILES: usize = 2000;
const SQL_CHUNK_SIZE: usize = 100;
const STREAM_BATCH_SIZE: usize = 15;

/// Find symbol usages and return all collected results at once.
/// 查找 symbol 使用位置，并一次性返回结果。
pub fn find_symbol_usages(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
) -> Result<Value> {
    let symbol_name = symbol_name.trim();

    if symbol_name.is_empty() {
        return Ok(json!({
            "results": [],
            "searched_files": 0,
            "found_definition": false,
        }));
    }

    let candidates = collect_candidate_files(conn, symbol_name, current_file)?;
    let mut results = Vec::new();

    for path in &candidates.file_paths {
        if results.len() >= MAX_RESULTS {
            break;
        }

        search_in_file(path, symbol_name, MAX_RESULTS - results.len(), |item| {
            results.push(item);
            Ok(())
        })?;
    }

    Ok(json!({
        "results": results,
        "searched_files": candidates.file_paths.len(),
        "found_definition": candidates.found_definition,
    }))
}

/// Find symbol usages and stream results in small batches.
/// 查找 symbol 使用位置，并以小批次流式返回结果。
pub fn find_symbol_usages_async<F>(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
    mut on_items: F,
) -> Result<Value>
where
    F: FnMut(Vec<Value>) -> Result<()>,
{
    let symbol_name = symbol_name.trim();

    if symbol_name.is_empty() {
        return Ok(json!({
            "searched_files": 0,
            "found_definition": false,
            "total_results": 0,
        }));
    }

    let candidates = collect_candidate_files(conn, symbol_name, current_file)?;
    let mut total_results = 0usize;
    let mut batch = Vec::new();

    for path in &candidates.file_paths {
        if total_results >= MAX_RESULTS {
            break;
        }

        search_in_file(path, symbol_name, MAX_RESULTS - total_results, |item| {
            batch.push(item);
            total_results += 1;

            if batch.len() >= STREAM_BATCH_SIZE {
                on_items(std::mem::take(&mut batch))?;
            }

            Ok(())
        })?;
    }

    if !batch.is_empty() {
        on_items(batch)?;
    }

    Ok(json!({
        "searched_files": candidates.file_paths.len(),
        "found_definition": candidates.found_definition,
        "total_results": total_results,
    }))
}

// -----------------------------------------------------------------------------
// Candidate collection
// -----------------------------------------------------------------------------

struct CandidateFiles {
    file_paths: Vec<String>,
    found_definition: bool,
}

/// Collect likely files where the symbol may be used.
/// 收集 symbol 可能出现的候选文件。
fn collect_candidate_files(
    conn: &Connection,
    symbol_name: &str,
    current_file: Option<&str>,
) -> Result<CandidateFiles> {
    let def_ids = find_definition_file_ids(conn, symbol_name)?;
    let found_definition = !def_ids.is_empty();

    let mut candidate_ids = HashSet::new();

    for id in &def_ids {
        candidate_ids.insert(*id);
    }

    for id in find_including_file_ids(conn, &def_ids)? {
        candidate_ids.insert(id);
    }

    if let Some(current) = current_file {
        if let Some(id) = find_file_id(conn, current)? {
            candidate_ids.insert(id);
        }
    }

    if candidate_ids.is_empty() {
        candidate_ids.extend(find_files_from_symbol_calls(conn, symbol_name, MAX_FILES)?);
    }

    let mut ids = candidate_ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(MAX_FILES);

    let mut file_paths = get_file_paths_by_ids(conn, &ids)?;
    file_paths.sort();
    file_paths.dedup();

    Ok(CandidateFiles {
        file_paths,
        found_definition,
    })
}

/// Find file ids where the symbol is defined as class or member.
/// 从 classes / members 表中查找 symbol 定义所在文件。
fn find_definition_file_ids(conn: &Connection, symbol_name: &str) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT c.file_id
        FROM classes c
        JOIN strings s ON c.name_id = s.id
        WHERE s.text = ?
          AND c.file_id IS NOT NULL
        "#,
        symbol_name,
        &mut seen,
        &mut ids,
    )?;

    collect_ids(
        conn,
        r#"
        SELECT DISTINCT COALESCE(m.file_id, c.file_id)
        FROM members m
        JOIN strings s ON m.name_id = s.id
        JOIN classes c ON m.class_id = c.id
        WHERE s.text = ?
          AND COALESCE(m.file_id, c.file_id) IS NOT NULL
        "#,
        symbol_name,
        &mut seen,
        &mut ids,
    )?;

    Ok(ids)
}

/// Find files that include the definition files.
/// 查找 include 了定义文件的文件。
fn find_including_file_ids(conn: &Connection, def_ids: &[i64]) -> Result<Vec<i64>> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    if def_ids.is_empty() {
        return Ok(results);
    }

    for chunk in def_ids.chunks(SQL_CHUNK_SIZE) {
        let placeholders = repeat_placeholders(chunk.len());
        let sql = format!(
            r#"
            SELECT DISTINCT fi.file_id
            FROM file_includes fi
            WHERE fi.resolved_file_id IN ({})
            "#,
            placeholders
        );

        let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;

        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            if seen.insert(id) {
                results.push(id);
            }
        }
    }

    Ok(results)
}

/// Fallback: find files from symbol_calls table.
/// 兜底：从 symbol_calls 表里找出现过这个 symbol 的文件。
fn find_files_from_symbol_calls(
    conn: &Connection,
    symbol_name: &str,
    limit: usize,
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT DISTINCT sc.file_id
        FROM symbol_calls sc
        JOIN strings s ON sc.name_id = s.id
        WHERE s.text = ?
        LIMIT ?
        "#,
    )?;

    let rows = stmt.query_map(rusqlite::params![symbol_name, limit as i64], |row| {
        row.get::<_, i64>(0)
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }

    Ok(results)
}

/// Find one file id from full path or filename.
/// 通过完整路径或文件名查找 file id。
fn find_file_id(conn: &Connection, file_path: &str) -> Result<Option<i64>> {
    let normalized = normalize_path(file_path);

    let sql = format!(
        r#"
        {}
        SELECT f.id
        FROM files f
        JOIN dir_paths dp ON f.directory_id = dp.id
        JOIN strings sn ON f.filename_id = sn.id
        WHERE dp.full_path || '/' || sn.text = ?
        LIMIT 1
        "#,
        PATH_CTE
    );

    if let Ok(id) = conn.query_row(&sql, [normalized], |row| row.get::<_, i64>(0)) {
        return Ok(Some(id));
    }

    let Some(filename) = std::path::Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return Ok(None);
    };

    let id = conn
        .query_row(
            r#"
            SELECT f.id
            FROM files f
            JOIN strings sn ON f.filename_id = sn.id
            WHERE sn.text = ?
            LIMIT 1
            "#,
            [filename],
            |row| row.get::<_, i64>(0),
        )
        .ok();

    Ok(id)
}

/// Collect ids from a simple one-parameter SQL query.
/// 从一个单参数 SQL 查询里收集 id。
fn collect_ids(
    conn: &Connection,
    sql: &str,
    symbol_name: &str,
    seen: &mut HashSet<i64>,
    ids: &mut Vec<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([symbol_name])?;

    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if seen.insert(id) {
            ids.push(id);
        }
    }

    Ok(())
}

/// Convert file ids to full file paths.
/// 把 file id 转换成完整文件路径。
fn get_file_paths_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<String>> {
    let mut results = Vec::new();

    if ids.is_empty() {
        return Ok(results);
    }

    for chunk in ids.chunks(SQL_CHUNK_SIZE) {
        let placeholders = repeat_placeholders(chunk.len());
        let sql = format!(
            r#"
            {}
            SELECT dp.full_path || '/' || sn.text AS path
            FROM files f
            JOIN dir_paths dp ON f.directory_id = dp.id
            JOIN strings sn ON f.filename_id = sn.id
            WHERE f.id IN ({})
            ORDER BY path
            "#,
            PATH_CTE,
            placeholders
        );

        let params = chunk.iter().map(|id| id as &dyn ToSql).collect::<Vec<_>>();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;

        while let Some(row) = rows.next()? {
            let path = row.get::<_, String>(0)?;
            results.push(normalize_path(&path));
        }
    }

    Ok(results)
}

// -----------------------------------------------------------------------------
// File text search
// -----------------------------------------------------------------------------

/// Search a single file line by line.
/// 逐行搜索单个文件。
fn search_in_file<F>(
    path: &str,
    symbol_name: &str,
    remaining_limit: usize,
    mut on_match: F,
) -> Result<()>
where
    F: FnMut(Value) -> Result<()>,
{
    if remaining_limit == 0 {
        return Ok(());
    }

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(()),
    };

    let reader = std::io::BufReader::new(file);
    let mut emitted = 0usize;

    for (line_index, line_result) in reader.lines().enumerate() {
        if emitted >= remaining_limit {
            break;
        }

        let line = match line_result {
            Ok(line) => line,
            Err(_) => continue,
        };

        let mut search_start = 0usize;

        while emitted < remaining_limit {
            let Some(col) = find_word_in_line_from(&line, symbol_name, search_start) else {
                break;
            };

            on_match(json!({
                "path": normalize_path(path),
                "line": line_index + 1,
                "col": col,
                "context": line.trim(),
            }))?;

            emitted += 1;
            search_start = col + symbol_name.len();
        }
    }

    Ok(())
}

/// Find a whole-word symbol occurrence in one line from an offset.
/// 从指定偏移开始，在一行里查找完整单词 symbol。
fn find_word_in_line_from(line: &str, symbol: &str, start_from: usize) -> Option<usize> {
    let symbol_len = symbol.len();

    if symbol_len == 0 || start_from >= line.len() {
        return None;
    }

    let bytes = line.as_bytes();
    let mut start = start_from;

    while start + symbol_len <= bytes.len() {
        let rel = line[start..].find(symbol)?;
        let abs = start + rel;

        if is_word_boundary(bytes, abs, symbol_len) {
            return Some(abs);
        }

        start = abs + 1;
    }

    None
}

/// Check whether the match has word boundaries on both sides.
/// 检查匹配结果两侧是否都是单词边界。
fn is_word_boundary(bytes: &[u8], start: usize, len: usize) -> bool {
    let end = start + len;

    let before_ok = start == 0 || !is_word_char(bytes[start - 1]);
    let after_ok = end >= bytes.len() || !is_word_char(bytes[end]);

    before_ok && after_ok
}

/// Return true for C/C++ identifier characters.
/// 判断是否是 C/C++ 标识符字符。
fn is_word_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_'
}

// -----------------------------------------------------------------------------
// Misc helpers
// -----------------------------------------------------------------------------

/// Create SQL placeholders like "?,?,?".
/// 生成 SQL 参数占位符，比如 "?,?,?"。
fn repeat_placeholders(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Normalize Windows paths to slash-separated paths.
/// 把 Windows 反斜杠路径统一成斜杠路径。
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").replace("//", "/")
}
