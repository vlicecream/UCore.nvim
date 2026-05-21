use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension};
use tracing::info;

use crate::types::{ParseResult, ProgressReporter};

pub const TEXT_DB_VERSION: i32 = 3;

const TEXT_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
const TEXT_DB_BULK_BUSY_TIMEOUT: Duration = Duration::from_millis(60_000);
const TEXT_INDEX_BULK_CHUNK_SIZE: usize = 500;
const TEXT_INDEX_DELTA_CHUNK_SIZE: usize = 250;

#[derive(Debug, Clone)]
pub struct TextIndexFile {
    pub path: String,
    pub extension: String,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct TextLineMatch {
    pub path: String,
    pub line_number: i64,
    pub line_text: String,
}

pub fn derived_text_db_path(primary_db_path: &str) -> String {
    let path = PathBuf::from(primary_db_path);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("ucore");
    parent
        .join(format!("{}-text.db", stem))
        .to_string_lossy()
        .to_string()
}

pub fn current_primary_db_path(conn: &Connection) -> Result<Option<String>> {
    let mut stmt = conn.prepare("PRAGMA database_list")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;

    for row in rows {
        let (name, path) = row?;
        if name == "main" {
            return Ok(path.filter(|value| !value.trim().is_empty()));
        }
    }

    Ok(None)
}

pub fn ensure_text_db(primary_db_path: &str) -> Result<String> {
    let text_db_path = derived_text_db_path(primary_db_path);
    ensure_text_db_version(&text_db_path)?;
    Ok(text_db_path)
}

pub fn init_text_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(TEXT_DB_BUSY_TIMEOUT)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS text_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS text_files (
            path TEXT PRIMARY KEY,
            path_lc TEXT NOT NULL,
            ext TEXT NOT NULL,
            mtime INTEGER
        );

        CREATE TABLE IF NOT EXISTS text_lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            line_number INTEGER NOT NULL,
            line_text TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS text_lines_fts USING fts5(
            line_text,
            content='text_lines',
            content_rowid='id',
            tokenize='trigram case_sensitive 0'
        );

        CREATE INDEX IF NOT EXISTS idx_text_files_path_lc ON text_files(path_lc);
        CREATE INDEX IF NOT EXISTS idx_text_files_ext ON text_files(ext);
        CREATE INDEX IF NOT EXISTS idx_text_lines_path ON text_lines(path);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_text_lines_path_line ON text_lines(path, line_number);
        "#,
    )?;

    create_text_line_sync_triggers(conn)?;

    conn.execute(
        "INSERT OR REPLACE INTO text_meta (key, value) VALUES ('db_version', ?1)",
        [TEXT_DB_VERSION.to_string()],
    )?;

    Ok(())
}

pub fn sync_text_files(
    primary_db_path: &str,
    files: &[TextIndexFile],
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let text_db_path = ensure_text_db(primary_db_path)?;
    let mut conn = Connection::open(&text_db_path)
        .with_context(|| format!("failed to open text database {}", text_db_path))?;
    init_text_db(&conn)?;

    let total = files.len();
    let started_at = Instant::now();
    prepare_text_bulk_write(&conn)?;
    let existing_count = count_text_files(&conn)?;
    let progress_total = total.max(1) + if existing_count == 0 { 2 } else { 1 };
    if let Some(reporter) = reporter {
        reporter.report("text_write", 0, progress_total, "Prepare");
    }

    if existing_count == 0 {
        sync_text_files_full(&mut conn, files, progress_total, reporter)?;
    } else {
        sync_text_files_delta(&mut conn, files, progress_total, reporter)?;
    }

    finalize_text_bulk_write(&conn)?;
    if let Some(reporter) = reporter {
        reporter.report("text_write", progress_total, progress_total, "Complete");
    }
    info!(
        "Text index sync finished in {} ms ({} files)",
        started_at.elapsed().as_millis(),
        total
    );
    Ok(())
}

pub fn sync_text_files_for_results(conn: &Connection, results: &[ParseResult]) -> Result<()> {
    let Some(primary_db_path) = current_primary_db_path(conn)? else {
        return Ok(());
    };

    let files = results
        .iter()
        .filter(|result| matches!(result.status.as_str(), "parsed" | "cache_hit"))
        .map(|result| {
            let extension = Path::new(&result.path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            TextIndexFile {
                path: result.path.clone(),
                extension,
                mtime: result.mtime as i64,
            }
        })
        .collect::<Vec<_>>();

    sync_text_files(&primary_db_path, &files, None)
}

pub fn delete_text_files(primary_db_path: &str, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    let text_db_path = derived_text_db_path(primary_db_path);
    if !Path::new(&text_db_path).is_file() {
        return Ok(());
    }

    let mut conn = Connection::open(&text_db_path)
        .with_context(|| format!("failed to open text database {}", text_db_path))?;
    init_text_db(&conn)?;
    let tx = conn.transaction()?;
    {
        let mut stmt_delete_lines = tx.prepare("DELETE FROM text_lines WHERE path = ?1")?;
        let mut stmt_delete_file = tx.prepare("DELETE FROM text_files WHERE path = ?1")?;

        for path in paths {
            let normalized = normalize_path(path);
            stmt_delete_lines.execute(params![normalized.as_str()])?;
            stmt_delete_file.execute(params![normalized.as_str()])?;
        }
    }

    tx.commit()?;
    Ok(())
}

fn sync_text_files_full(
    conn: &mut Connection,
    files: &[TextIndexFile],
    progress_total: usize,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    reset_text_index_storage(conn)?;

    let file_total = files.len().max(1);
    for (chunk_index, chunk) in files.chunks(TEXT_INDEX_BULK_CHUNK_SIZE).enumerate() {
        let chunk_started_at = Instant::now();
        let tx = conn.transaction()?;
        {
            let mut stmt_insert_file = tx.prepare(
                "INSERT INTO text_files (path, path_lc, ext, mtime) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut stmt_insert_line = tx.prepare(
                "INSERT INTO text_lines (path, line_number, line_text) VALUES (?1, ?2, ?3)",
            )?;

            for (index, file) in chunk.iter().enumerate() {
                let current = chunk_index * TEXT_INDEX_BULK_CHUNK_SIZE + index + 1;
                if let Some(reporter) = reporter {
                    if current == 1 || current == file_total || current % 200 == 0 {
                        reporter.report("text_write", current, progress_total, &format!("{}/{}", current, file_total));
                    }
                }

                insert_text_file_records(&mut stmt_insert_file, &mut stmt_insert_line, file)?;
            }
        }
        tx.commit()?;
        info!(
            "Text index bulk chunk {}/{} finished in {} ms ({} files)",
            chunk_index + 1,
            files.len().div_ceil(TEXT_INDEX_BULK_CHUNK_SIZE),
            chunk_started_at.elapsed().as_millis(),
            chunk.len()
        );
    }

    if let Some(reporter) = reporter {
        reporter.report("text_write", file_total, progress_total, "Build FTS");
    }
    let rebuild_started_at = Instant::now();
    rebuild_text_line_fts(conn)?;
    if let Some(reporter) = reporter {
        reporter.report("text_write", file_total + 1, progress_total, "Optimize");
    }
    info!(
        "Text index FTS rebuild finished in {} ms",
        rebuild_started_at.elapsed().as_millis()
    );
    Ok(())
}

fn sync_text_files_delta(
    conn: &mut Connection,
    files: &[TextIndexFile],
    progress_total: usize,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<()> {
    let file_total = files.len().max(1);
    for (chunk_index, chunk) in files.chunks(TEXT_INDEX_DELTA_CHUNK_SIZE).enumerate() {
        let chunk_started_at = Instant::now();
        let tx = conn.transaction()?;
        {
            let mut stmt_delete_lines = tx.prepare("DELETE FROM text_lines WHERE path = ?1")?;
            let mut stmt_delete_file = tx.prepare("DELETE FROM text_files WHERE path = ?1")?;
            let mut stmt_insert_file = tx.prepare(
                "INSERT INTO text_files (path, path_lc, ext, mtime) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut stmt_insert_line = tx.prepare(
                "INSERT INTO text_lines (path, line_number, line_text) VALUES (?1, ?2, ?3)",
            )?;

            for (index, file) in chunk.iter().enumerate() {
                let current = chunk_index * TEXT_INDEX_DELTA_CHUNK_SIZE + index + 1;
                if let Some(reporter) = reporter {
                    if current == 1 || current == file_total || current % 200 == 0 {
                        reporter.report("text_write", current, progress_total, &format!("{}/{}", current, file_total));
                    }
                }

                let normalized_path = normalize_path(&file.path);
                stmt_delete_lines.execute(params![normalized_path.as_str()])?;
                stmt_delete_file.execute(params![normalized_path.as_str()])?;
                insert_text_file_records(&mut stmt_insert_file, &mut stmt_insert_line, file)?;
            }
        }
        tx.commit()?;
        info!(
            "Text index delta chunk {}/{} finished in {} ms ({} files)",
            chunk_index + 1,
            files.len().div_ceil(TEXT_INDEX_DELTA_CHUNK_SIZE),
            chunk_started_at.elapsed().as_millis(),
            chunk.len()
        );
    }

    if let Some(reporter) = reporter {
        reporter.report("text_write", file_total, progress_total, "Optimize");
    }

    Ok(())
}

fn insert_text_file_records(
    stmt_insert_file: &mut rusqlite::Statement<'_>,
    stmt_insert_line: &mut rusqlite::Statement<'_>,
    file: &TextIndexFile,
) -> Result<()> {
    let normalized_path = normalize_path(&file.path);
    if !should_index_text_file(&normalized_path, &file.extension) {
        return Ok(());
    }

    let file_handle = match File::open(&normalized_path) {
        Ok(file_handle) => file_handle,
        Err(_) => return Ok(()),
    };

    stmt_insert_file.execute(params![
        normalized_path.as_str(),
        normalized_path.to_ascii_lowercase(),
        file.extension.to_ascii_lowercase(),
        file.mtime,
    ])?;

    let mut reader = BufReader::new(file_handle);
    let mut line = Vec::new();
    let mut line_number = 0i64;

    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }

        line_number += 1;
        let line_text = decode_text_line_lossy(&line);
        stmt_insert_line.execute(params![
            normalized_path.as_str(),
            line_number,
            trim_line_ending(&line_text),
        ])?;
    }

    Ok(())
}

fn reset_text_index_storage(conn: &mut Connection) -> Result<()> {
    drop_text_line_sync_triggers(conn)?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM text_files", [])?;
    tx.execute("DELETE FROM text_lines", [])?;
    tx.commit()?;
    recreate_text_lines_fts(conn)?;
    Ok(())
}

fn count_text_files(conn: &Connection) -> Result<usize> {
    Ok(conn.query_row("SELECT COUNT(1) FROM text_files", [], |row| row.get::<_, i64>(0))? as usize)
}

fn trim_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

pub fn search_matching_paths(primary_conn: &Connection, pattern: &str, limit: usize) -> Result<Vec<String>> {
    let Some(conn) = open_text_db_read_only_for_primary(primary_conn)? else {
        return Ok(Vec::new());
    };

    let needle = pattern.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let limit = limit.clamp(1, 10_000);
    if needle.len() < 3 {
        return collect_all_paths(&conn, limit);
    }

    let query = quoted_match_query(needle);
    let candidate_limit = limit.saturating_mul(32).clamp(64, 8192);
    let mut stmt = conn.prepare(
        "SELECT tl.path
         FROM text_lines_fts
         JOIN text_lines tl ON tl.id = text_lines_fts.rowid
         WHERE text_lines_fts MATCH ?1
         ORDER BY bm25(text_lines_fts), tl.path, tl.line_number
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, candidate_limit as i64], |row| row.get::<_, String>(0))?;
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let path = normalize_path(&row?);
        if seen.insert(path.clone()) {
            paths.push(path);
            if paths.len() >= limit {
                break;
            }
        }
    }
    Ok(paths)
}

pub fn search_matching_lines(
    primary_conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<TextLineMatch>> {
    let Some(conn) = open_text_db_read_only_for_primary(primary_conn)? else {
        return Ok(Vec::new());
    };
    let needle = pattern.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    search_matching_lines_from_conn(&conn, needle, limit, offset)
}

pub fn search_matching_lines_in_paths(
    primary_conn: &Connection,
    pattern: &str,
    allowed_paths: &[String],
    limit: usize,
    offset: usize,
) -> Result<Vec<TextLineMatch>> {
    let Some(conn) = open_text_db_read_only_for_primary(primary_conn)? else {
        return Ok(Vec::new());
    };

    let needle = pattern.trim();
    if needle.is_empty() || allowed_paths.is_empty() {
        return Ok(Vec::new());
    }

    if needle.len() < 3 {
        return collect_line_matches_from_paths_fallback(allowed_paths, needle, limit, offset);
    }

    let mut results = Vec::new();
    let mut skipped = 0usize;
    for chunk in allowed_paths.chunks(200) {
        if results.len() >= limit {
            break;
        }

        let mut sql = String::from(
            "SELECT tl.path, tl.line_number, tl.line_text
             FROM text_lines_fts
             JOIN text_lines tl ON tl.id = text_lines_fts.rowid
             WHERE text_lines_fts MATCH ?1
               AND tl.path IN (",
        );
        for (index, _) in chunk.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            sql.push_str(&(index + 2).to_string());
        }
        sql.push_str(
            ")
             ORDER BY bm25(text_lines_fts), tl.path, tl.line_number",
        );

        let mut stmt = conn.prepare(&sql)?;
        let mut params = Vec::with_capacity(chunk.len() + 1);
        params.push(SqlValue::Text(quoted_match_query(needle)));
        for path in chunk {
            params.push(SqlValue::Text(normalize_path(path)));
        }

        let rows = stmt.query_map(params_from_iter(params), |row| {
            Ok(TextLineMatch {
                path: normalize_path(&row.get::<_, String>(0)?),
                line_number: row.get::<_, i64>(1)?,
                line_text: row.get::<_, String>(2)?,
            })
        })?;

        for row in rows {
            if skipped < offset {
                skipped += 1;
                continue;
            }
            results.push(row?);
            if results.len() >= limit {
                break;
            }
        }
    }

    Ok(results)
}

pub fn read_line_at(path: &str, line_number: usize) -> Option<String> {
    if line_number == 0 {
        return None;
    }

    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut index = 0usize;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).ok()?;
        if read == 0 {
            break;
        }
        index += 1;
        if index == line_number {
            return Some(trim_line_ending(&decode_text_line_lossy(&line)).to_string());
        }
    }

    None
}

fn ensure_text_db_version(text_db_path: &str) -> Result<bool> {
    let db_exists = Path::new(text_db_path).exists();

    if db_exists && text_db_version_matches(text_db_path)? {
        return Ok(false);
    }

    if db_exists {
        std::fs::remove_file(text_db_path)
            .with_context(|| format!("failed to remove old text database {}", text_db_path))?;
    }

    let conn = Connection::open(text_db_path)
        .with_context(|| format!("failed to open text database {}", text_db_path))?;
    init_text_db(&conn)?;
    Ok(true)
}

fn text_db_version_matches(text_db_path: &str) -> Result<bool> {
    let conn = Connection::open(text_db_path)
        .with_context(|| format!("failed to open text database {}", text_db_path))?;
    let version = conn
        .query_row(
            "SELECT value FROM text_meta WHERE key = 'db_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i32>().ok());
    Ok(version == Some(TEXT_DB_VERSION))
}

fn open_text_db_read_only_for_primary(primary_conn: &Connection) -> Result<Option<Connection>> {
    let Some(primary_db_path) = current_primary_db_path(primary_conn)? else {
        return Ok(None);
    };

    let text_db_path = derived_text_db_path(&primary_db_path);
    if !Path::new(&text_db_path).is_file() {
        return Ok(None);
    }

    let conn = Connection::open_with_flags(
        &text_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open text database {}", text_db_path))?;
    conn.busy_timeout(TEXT_DB_BUSY_TIMEOUT)?;
    Ok(Some(conn))
}

fn collect_all_paths(conn: &Connection, limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT path
         FROM text_files
         ORDER BY path_lc
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| row.get::<_, String>(0))?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(normalize_path(&row?));
    }
    Ok(paths)
}

fn search_matching_lines_from_conn(
    conn: &Connection,
    needle: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<TextLineMatch>> {
    if needle.len() < 3 {
        let target = limit.saturating_add(offset).clamp(1, 5_000);
        let candidate_limit = target.saturating_mul(8).clamp(64, 4096);
        let candidate_paths = collect_all_paths(conn, candidate_limit)?;
        return collect_line_matches_from_paths_fallback(&candidate_paths, needle, limit, offset);
    }

    let mut stmt = conn.prepare(
        "SELECT tl.path, tl.line_number, tl.line_text
         FROM text_lines_fts
         JOIN text_lines tl ON tl.id = text_lines_fts.rowid
         WHERE text_lines_fts MATCH ?1
         ORDER BY bm25(text_lines_fts), tl.path, tl.line_number
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(
        params![quoted_match_query(needle), limit.clamp(1, 5_000) as i64, offset.min(1_000_000) as i64],
        |row| {
            Ok(TextLineMatch {
                path: normalize_path(&row.get::<_, String>(0)?),
                line_number: row.get::<_, i64>(1)?,
                line_text: row.get::<_, String>(2)?,
            })
        },
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn collect_line_matches_from_paths_fallback(
    paths: &[String],
    needle: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<TextLineMatch>> {
    let needle_lower = needle.to_ascii_lowercase();
    let mut skipped = 0usize;
    let mut results = Vec::new();
    for path in paths {
        if results.len() >= limit {
            break;
        }

        collect_line_matches_from_file(
            path,
            &needle_lower,
            offset,
            &mut skipped,
            limit,
            &mut results,
        )?;
    }
    Ok(results)
}

fn collect_line_matches_from_file(
    path: &str,
    needle_lower: &str,
    offset: usize,
    skipped: &mut usize,
    limit: usize,
    results: &mut Vec<TextLineMatch>,
) -> Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(()),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let normalized_path = normalize_path(path);
    let mut line_number = 0usize;

    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }

        line_number += 1;
        let line_text = decode_text_line_lossy(&line);
        if !line_text.to_ascii_lowercase().contains(needle_lower) {
            continue;
        }

        if *skipped < offset {
            *skipped += 1;
            continue;
        }

        results.push(TextLineMatch {
            path: normalized_path.clone(),
            line_number: line_number as i64,
            line_text: trim_line_ending(&line_text).to_string(),
        });

        if results.len() >= limit {
            break;
        }
    }

    Ok(())
}

fn decode_text_line_lossy(line: &[u8]) -> String {
    String::from_utf8_lossy(line).into_owned()
}

fn quoted_match_query(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

fn create_text_lines_fts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS text_lines_fts USING fts5(
            line_text,
            content='text_lines',
            content_rowid='id',
            tokenize='trigram case_sensitive 0'
        );
        "#,
    )?;
    Ok(())
}

fn recreate_text_lines_fts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS text_lines_fts;
        "#,
    )?;
    create_text_lines_fts(conn)?;
    Ok(())
}

fn create_text_line_sync_triggers(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TRIGGER IF NOT EXISTS text_lines_ai AFTER INSERT ON text_lines BEGIN
            INSERT INTO text_lines_fts(rowid, line_text)
            VALUES (new.id, new.line_text);
        END;

        CREATE TRIGGER IF NOT EXISTS text_lines_ad AFTER DELETE ON text_lines BEGIN
            INSERT INTO text_lines_fts(text_lines_fts, rowid, line_text)
            VALUES ('delete', old.id, old.line_text);
        END;

        CREATE TRIGGER IF NOT EXISTS text_lines_au AFTER UPDATE ON text_lines BEGIN
            INSERT INTO text_lines_fts(text_lines_fts, rowid, line_text)
            VALUES ('delete', old.id, old.line_text);
            INSERT INTO text_lines_fts(rowid, line_text)
            VALUES (new.id, new.line_text);
        END;
        "#,
    )?;
    Ok(())
}

fn drop_text_line_sync_triggers(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        DROP TRIGGER IF EXISTS text_lines_ai;
        DROP TRIGGER IF EXISTS text_lines_ad;
        DROP TRIGGER IF EXISTS text_lines_au;
        "#,
    )?;
    Ok(())
}

fn rebuild_text_line_fts(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO text_lines_fts(text_lines_fts) VALUES('rebuild')", [])?;
    create_text_line_sync_triggers(conn)?;
    Ok(())
}

fn prepare_text_bulk_write(conn: &Connection) -> Result<()> {
    conn.busy_timeout(TEXT_DB_BULK_BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", "-400000")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

fn finalize_text_bulk_write(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO text_lines_fts(text_lines_fts) VALUES('optimize')", [])?;
    conn.execute("PRAGMA optimize", [])?;
    Ok(())
}

fn should_index_text_file(path: &str, extension: &str) -> bool {
    if !is_search_text_extension(extension) {
        return false;
    }

    let lowered = path.to_ascii_lowercase();
    !lowered.contains("/intermediate/")
        && !lowered.contains("/binaries/")
        && !lowered.contains("/deriveddatacache/")
        && !lowered.contains("/thirdparty/")
        && !lowered.contains("/source/thirdparty/")
        && !lowered.contains("/framework/libs/")
        && !lowered.contains("/external/")
}

fn is_search_text_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "h" | "hh" | "hpp" | "hxx" | "c" | "cc" | "cpp" | "cxx" | "inl" | "ipp" | "cs"
            | "ini" | "json" | "uproject" | "uplugin"
    )
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_base(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ucore-text-db-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn create_primary_db(base: &Path) -> (Connection, PathBuf) {
        fs::create_dir_all(base).unwrap();
        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        (conn, db_path)
    }

    #[test]
    fn search_matching_paths_reads_from_line_index() {
        let base = temp_base("paths");
        let (conn, db_path) = create_primary_db(&base);
        let source_path = base.join("GameplayAbility.cpp");
        fs::write(&source_path, "void Test() {\n    GameplayCue();\n}\n").unwrap();

        sync_text_files(
            db_path.to_string_lossy().as_ref(),
            &[TextIndexFile {
                path: source_path.to_string_lossy().to_string(),
                extension: "cpp".to_string(),
                mtime: 1,
            }],
            None,
        )
        .unwrap();

        let paths = search_matching_paths(&conn, "GameplayCue", 20).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], source_path.to_string_lossy().replace('\\', "/"));

        let _ = fs::remove_file(derived_text_db_path(db_path.to_string_lossy().as_ref()));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(&source_path);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn sync_text_files_updates_and_deletes_line_hits() {
        let base = temp_base("delta");
        let (conn, db_path) = create_primary_db(&base);
        let source_path = base.join("Widget.cpp");
        let source_path_text = source_path.to_string_lossy().to_string();

        fs::write(&source_path, "AlphaCall();\n").unwrap();
        sync_text_files(
            db_path.to_string_lossy().as_ref(),
            &[TextIndexFile {
                path: source_path_text.clone(),
                extension: "cpp".to_string(),
                mtime: 1,
            }],
            None,
        )
        .unwrap();

        let alpha_hits = search_matching_lines(&conn, "AlphaCall", 20, 0).unwrap();
        assert_eq!(alpha_hits.len(), 1);

        fs::write(&source_path, "BetaCall();\n").unwrap();
        sync_text_files(
            db_path.to_string_lossy().as_ref(),
            &[TextIndexFile {
                path: source_path_text.clone(),
                extension: "cpp".to_string(),
                mtime: 2,
            }],
            None,
        )
        .unwrap();

        assert!(search_matching_lines(&conn, "AlphaCall", 20, 0).unwrap().is_empty());
        let beta_hits = search_matching_lines(&conn, "BetaCall", 20, 0).unwrap();
        assert_eq!(beta_hits.len(), 1);

        delete_text_files(db_path.to_string_lossy().as_ref(), &[source_path_text]).unwrap();
        assert!(search_matching_lines(&conn, "BetaCall", 20, 0).unwrap().is_empty());

        let _ = fs::remove_file(derived_text_db_path(db_path.to_string_lossy().as_ref()));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(&source_path);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn sync_text_files_tolerates_non_utf8_lines() {
        let base = temp_base("nonutf8");
        let (conn, db_path) = create_primary_db(&base);
        let source_path = base.join("AnsiLike.cpp");
        let source_path_text = source_path.to_string_lossy().to_string();

        fs::write(&source_path, b"AlphaCall();\nBeta:\xFF\xFE\x80\n").unwrap();
        sync_text_files(
            db_path.to_string_lossy().as_ref(),
            &[TextIndexFile {
                path: source_path_text.clone(),
                extension: "cpp".to_string(),
                mtime: 1,
            }],
            None,
        )
        .unwrap();

        let alpha_hits = search_matching_lines(&conn, "AlphaCall", 20, 0).unwrap();
        assert_eq!(alpha_hits.len(), 1);
        let beta_line = read_line_at(&source_path_text, 2).unwrap();
        assert!(beta_line.contains("Beta:"));

        let _ = fs::remove_file(derived_text_db_path(db_path.to_string_lossy().as_ref()));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(&source_path);
        let _ = fs::remove_dir(&base);
    }
}
