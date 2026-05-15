use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use tracing::info;

use crate::types::{ParseResult, ProgressReporter};

pub const TEXT_DB_VERSION: i32 = 1;

const TEXT_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
const TEXT_DB_BULK_BUSY_TIMEOUT: Duration = Duration::from_millis(60_000);
const TEXT_INDEX_CHUNK_SIZE: usize = 250;

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

        CREATE VIRTUAL TABLE IF NOT EXISTS text_files_fts USING fts5(
            path UNINDEXED,
            content,
            tokenize='trigram case_sensitive 0'
        );

        CREATE INDEX IF NOT EXISTS idx_text_files_path_lc ON text_files(path_lc);
        CREATE INDEX IF NOT EXISTS idx_text_files_ext ON text_files(ext);
        "#,
    )?;

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
    if let Some(reporter) = reporter {
        reporter.report("text_write", 0, total.max(1), "Prepare");
    }

    for (chunk_index, chunk) in files.chunks(TEXT_INDEX_CHUNK_SIZE).enumerate() {
        let chunk_started_at = Instant::now();
        let tx = conn.transaction()?;
        {
            let mut stmt_delete_fts = tx.prepare(
                "DELETE FROM text_files_fts
                 WHERE rowid IN (SELECT rowid FROM text_files WHERE path = ?1)",
            )?;
            let mut stmt_delete_file = tx.prepare("DELETE FROM text_files WHERE path = ?1")?;
            let mut stmt_insert_file = tx.prepare(
                "INSERT INTO text_files (path, path_lc, ext, mtime) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut stmt_insert_fts = tx.prepare(
                "INSERT INTO text_files_fts (rowid, path, content) VALUES (?1, ?2, ?3)",
            )?;

            for (index, file) in chunk.iter().enumerate() {
                let current = chunk_index * TEXT_INDEX_CHUNK_SIZE + index + 1;
                if let Some(reporter) = reporter {
                    if current == 1 || current == total || current % 200 == 0 {
                        reporter.report("text_write", current, total, &format!("{}/{}", current, total));
                    }
                }

                let normalized_path = normalize_path(&file.path);
                stmt_delete_fts.execute(params![normalized_path.as_str()])?;
                stmt_delete_file.execute(params![normalized_path.as_str()])?;

                if !should_index_text_file(&normalized_path, &file.extension) {
                    continue;
                }

                let content = match std::fs::read_to_string(&normalized_path) {
                    Ok(content) => content,
                    Err(_) => continue,
                };

                if content.trim().is_empty() {
                    continue;
                }

                stmt_insert_file.execute(params![
                    normalized_path.as_str(),
                    normalized_path.to_ascii_lowercase(),
                    file.extension.to_ascii_lowercase(),
                    file.mtime,
                ])?;

                let rowid = tx.last_insert_rowid();
                stmt_insert_fts.execute(params![rowid, normalized_path.as_str(), content])?;
            }
        }

        tx.commit()?;
        info!(
            "Text index chunk {}/{} finished in {} ms ({} files)",
            chunk_index + 1,
            files.len().div_ceil(TEXT_INDEX_CHUNK_SIZE),
            chunk_started_at.elapsed().as_millis(),
            chunk.len()
        );
    }

    finalize_text_bulk_write(&conn)?;
    if let Some(reporter) = reporter {
        reporter.report("text_write", total.max(1), total.max(1), "Complete");
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
        let mut stmt_delete_fts = tx.prepare(
            "DELETE FROM text_files_fts
             WHERE rowid IN (SELECT rowid FROM text_files WHERE path = ?1)",
        )?;
        let mut stmt_delete_file = tx.prepare("DELETE FROM text_files WHERE path = ?1")?;

        for path in paths {
            let normalized = normalize_path(path);
            stmt_delete_fts.execute(params![normalized.as_str()])?;
            stmt_delete_file.execute(params![normalized.as_str()])?;
        }
    }

    tx.commit()?;
    Ok(())
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
    let mut stmt = conn.prepare(
        "SELECT path
         FROM text_files_fts
         WHERE text_files_fts MATCH ?1
         ORDER BY bm25(text_files_fts), path
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| row.get::<_, String>(0))?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(normalize_path(&row?));
    }
    Ok(paths)
}

pub fn search_matching_lines(
    primary_conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<TextLineMatch>> {
    let needle = pattern.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let target = limit.saturating_add(offset).clamp(1, 5_000);
    let candidate_limit = target.saturating_mul(8).clamp(64, 4096);
    let candidate_paths = search_matching_paths(primary_conn, needle, candidate_limit)?;
    let needle_lower = needle.to_ascii_lowercase();
    let mut skipped = 0usize;
    let mut results = Vec::new();

    for path in candidate_paths {
        if results.len() >= limit {
            break;
        }

        collect_line_matches_from_file(
            &path,
            &needle_lower,
            offset,
            &mut skipped,
            limit,
            &mut results,
        )?;
    }

    Ok(results)
}

pub fn read_line_at(path: &str, line_number: usize) -> Option<String> {
    if line_number == 0 {
        return None;
    }

    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (index, line) in reader.lines().enumerate() {
        if index + 1 == line_number {
            return line.ok();
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
    let mut line = String::new();
    let normalized_path = normalize_path(path);
    let mut line_number = 0usize;

    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }

        line_number += 1;
        if !line.to_ascii_lowercase().contains(needle_lower) {
            continue;
        }

        if *skipped < offset {
            *skipped += 1;
            continue;
        }

        results.push(TextLineMatch {
            path: normalized_path.clone(),
            line_number: line_number as i64,
            line_text: line.trim_end().to_string(),
        });

        if results.len() >= limit {
            break;
        }
    }

    Ok(())
}

fn quoted_match_query(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
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
