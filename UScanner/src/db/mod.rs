pub mod project_path;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::info;

use crate::types::{ParseResult, ProgressReporter};

/// Main SQLite schema version.
/// 主数据库 schema 版本。
///
/// Increment this when table structures, indexes, or stored data semantics change.
/// 当表结构、索引或存储语义变化时递增。
pub const DB_VERSION: i32 = 25;

/// Completion cache version.
/// 补全缓存版本。
///
/// Increment this when completion logic changes but the main DB schema does not.
/// 当补全逻辑变化但主数据库 schema 不变时递增。
pub const COMPLETION_CACHE_VERSION: i32 = 4;

/// SQLite busy timeout for normal operations.
/// 普通数据库操作的 busy timeout。
const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// SQLite busy timeout for bulk writes.
/// 批量写入时的 busy timeout。
const DB_BULK_BUSY_TIMEOUT: Duration = Duration::from_millis(60_000);
const ITEM_PROGRESS_EVERY: usize = 250;

/// Ensure the on-disk database matches the current schema version.
/// 确保磁盘数据库版本和当前 schema 版本一致。
///
/// Returns true when the database was newly initialized or rebuilt.
/// 如果数据库被新建或重建，返回 true。
pub fn ensure_correct_version(db_path: &str) -> anyhow::Result<bool> {
    let db_exists = Path::new(db_path).exists();

    if db_exists && database_version_matches(db_path)? {
        return Ok(false);
    }

    if db_exists {
        info!(
            "DB version mismatch or missing. Re-initializing {} with version {}.",
            db_path, DB_VERSION
        );

        std::fs::remove_file(db_path)
            .with_context(|| format!("failed to remove old database {}", db_path))?;
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("failed to open database {}", db_path))?;
    init_db(&conn)?;

    Ok(true)
}

/// Check whether an existing database has the expected schema version.
/// 检查现有数据库是否是预期 schema 版本。
fn database_version_matches(db_path: &str) -> anyhow::Result<bool> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed to open database {}", db_path))?;

    let version = conn
        .query_row(
            "SELECT value FROM project_meta WHERE key = 'db_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i32>().ok());

    Ok(version == Some(DB_VERSION))
}

/// Initialize all database tables, indexes, and metadata.
/// 初始化所有数据库表、索引和元数据。
pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(DB_BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    create_tables(conn)?;
    create_views(conn)?;
    create_indices(conn)?;

    conn.execute(
        "INSERT OR REPLACE INTO project_meta (key, value) VALUES ('db_version', ?1)",
        [DB_VERSION.to_string()],
    )?;

    Ok(())
}

/// Create all tables used by UCore's project index.
/// 创建 UCore 项目索引用到的全部表。
fn create_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS strings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS directories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            parent_id INTEGER,
            name_id INTEGER NOT NULL,
            UNIQUE(parent_id, name_id),
            FOREIGN KEY(parent_id) REFERENCES directories(id) ON DELETE CASCADE,
            FOREIGN KEY(name_id) REFERENCES strings(id)
        );

        CREATE TABLE IF NOT EXISTS modules (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name_id INTEGER NOT NULL,
            type TEXT,
            scope TEXT,
            root_directory_id INTEGER NOT NULL,
            build_cs_path TEXT,
            owner_name TEXT,
            component_name TEXT,
            deep_dependencies TEXT,
            UNIQUE(name_id, root_directory_id),
            FOREIGN KEY(name_id) REFERENCES strings(id),
            FOREIGN KEY(root_directory_id) REFERENCES directories(id)
        );

        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            directory_id INTEGER NOT NULL,
            filename_id INTEGER NOT NULL,
            extension TEXT,
            mtime INTEGER,
            module_id INTEGER,
            is_header INTEGER DEFAULT 0,
            file_hash TEXT,
            UNIQUE(directory_id, filename_id),
            FOREIGN KEY(directory_id) REFERENCES directories(id) ON DELETE CASCADE,
            FOREIGN KEY(filename_id) REFERENCES strings(id),
            FOREIGN KEY(module_id) REFERENCES modules(id)
        );

        CREATE TABLE IF NOT EXISTS classes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name_id INTEGER NOT NULL,
            namespace_id INTEGER,
            base_class_id INTEGER,
            file_id INTEGER,
            line_number INTEGER,
            end_line_number INTEGER,
            symbol_type TEXT DEFAULT 'class',
            FOREIGN KEY(name_id) REFERENCES strings(id),
            FOREIGN KEY(namespace_id) REFERENCES strings(id),
            FOREIGN KEY(base_class_id) REFERENCES strings(id),
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS members (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            class_id INTEGER NOT NULL,
            name_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            flags TEXT,
            access TEXT,
            detail TEXT,
            return_type_id INTEGER,
            is_static INTEGER,
            line_number INTEGER,
            file_id INTEGER,
            FOREIGN KEY(class_id) REFERENCES classes(id) ON DELETE CASCADE,
            FOREIGN KEY(name_id) REFERENCES strings(id),
            FOREIGN KEY(type_id) REFERENCES strings(id),
            FOREIGN KEY(return_type_id) REFERENCES strings(id),
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS enum_values (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            enum_id INTEGER NOT NULL,
            name_id INTEGER NOT NULL,
            line_number INTEGER,
            file_id INTEGER,
            FOREIGN KEY(enum_id) REFERENCES classes(id) ON DELETE CASCADE,
            FOREIGN KEY(name_id) REFERENCES strings(id),
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS inheritance (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            child_id INTEGER NOT NULL,
            parent_name_id INTEGER NOT NULL,
            parent_class_id INTEGER,
            FOREIGN KEY(child_id) REFERENCES classes(id) ON DELETE CASCADE,
            FOREIGN KEY(parent_name_id) REFERENCES strings(id),
            FOREIGN KEY(parent_class_id) REFERENCES classes(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS symbol_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            line INTEGER NOT NULL,
            name_id INTEGER NOT NULL,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
            FOREIGN KEY(name_id) REFERENCES strings(id)
        );

        CREATE TABLE IF NOT EXISTS file_includes (
            file_id INTEGER NOT NULL,
            include_path_id INTEGER NOT NULL,
            base_filename_id INTEGER NOT NULL,
            resolved_file_id INTEGER,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
            FOREIGN KEY(include_path_id) REFERENCES strings(id),
            FOREIGN KEY(base_filename_id) REFERENCES strings(id),
            FOREIGN KEY(resolved_file_id) REFERENCES files(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS components (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            display_name TEXT,
            type TEXT,
            owner_name TEXT,
            root_path TEXT,
            uplugin_path TEXT,
            uproject_path TEXT,
            engine_association TEXT
        );

        CREATE TABLE IF NOT EXISTS project_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS persistent_cache (
            key TEXT PRIMARY KEY,
            value BLOB NOT NULL,
            hit_count INTEGER DEFAULT 1,
            last_used INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS cache_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS assets (
            asset_path TEXT PRIMARY KEY,
            asset_key TEXT NOT NULL,
            source_path TEXT NOT NULL UNIQUE,
            source_path_key TEXT NOT NULL UNIQUE,
            parent_class TEXT,
            parent_class_key TEXT,
            mtime INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS asset_references (
            asset_path TEXT NOT NULL,
            reference_key TEXT NOT NULL,
            FOREIGN KEY(asset_path) REFERENCES assets(asset_path) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS asset_functions (
            asset_path TEXT NOT NULL,
            function_key TEXT NOT NULL,
            FOREIGN KEY(asset_path) REFERENCES assets(asset_path) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS gameplay_tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            identifier TEXT NOT NULL,
            tag_path TEXT,
            kind TEXT NOT NULL,
            line_number INTEGER,
            file_id INTEGER NOT NULL,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS macro_definitions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            line_number INTEGER,
            file_id INTEGER NOT NULL,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS search_files (
            file_id INTEGER PRIMARY KEY,
            module_id INTEGER,
            module_name TEXT,
            module_name_lc TEXT,
            path TEXT NOT NULL,
            path_lc TEXT NOT NULL,
            basename TEXT NOT NULL,
            basename_lc TEXT NOT NULL,
            ext TEXT,
            is_source INTEGER NOT NULL DEFAULT 0,
            is_header INTEGER NOT NULL DEFAULT 0,
            is_searchable_text INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS search_symbols (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            symbol_rowid INTEGER NOT NULL,
            symbol_table TEXT NOT NULL,
            kind TEXT NOT NULL,
            kind_rank INTEGER NOT NULL,
            name TEXT NOT NULL,
            name_lc TEXT NOT NULL,
            compact_name TEXT NOT NULL,
            owner_name TEXT,
            owner_name_lc TEXT,
            module_id INTEGER,
            module_name TEXT,
            module_name_lc TEXT,
            path TEXT NOT NULL,
            path_lc TEXT NOT NULL,
            basename TEXT NOT NULL,
            basename_lc TEXT NOT NULL,
            ext TEXT,
            line_number INTEGER,
            is_class_like INTEGER NOT NULL DEFAULT 0,
            is_function_like INTEGER NOT NULL DEFAULT 0,
            is_member_like INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS search_text_lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            module_id INTEGER,
            module_name TEXT,
            module_name_lc TEXT,
            path TEXT NOT NULL,
            path_lc TEXT NOT NULL,
            line_number INTEGER NOT NULL,
            line_text TEXT NOT NULL,
            line_text_lc TEXT NOT NULL,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS search_symbol_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            module_id INTEGER,
            module_name TEXT,
            module_name_lc TEXT,
            path TEXT NOT NULL,
            path_lc TEXT NOT NULL,
            line_number INTEGER NOT NULL,
            name TEXT NOT NULL,
            name_lc TEXT NOT NULL,
            FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS search_asset_usages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            lookup_key TEXT NOT NULL,
            usage_kind TEXT NOT NULL,
            asset_path TEXT NOT NULL,
            asset_name TEXT NOT NULL,
            asset_name_lc TEXT NOT NULL,
            source_path TEXT NOT NULL
        );
        "#,
    )?;

    let _ = conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
            name,
            type,
            class_name UNINDEXED,
            rowid_ref UNINDEXED
        )",
        [],
    );

    Ok(())
}

/// Create query helper views.
/// 创建查询辅助视图。
fn create_views(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIEW IF NOT EXISTS dir_paths AS
        WITH RECURSIVE paths(id, full_path) AS (
            SELECT
                d.id,
                s.text
            FROM directories d
            JOIN strings s ON d.name_id = s.id
            WHERE d.parent_id IS NULL

            UNION ALL

            SELECT
                d.id,
                CASE
                    WHEN paths.full_path = '/'
                        THEN '/' || s.text
                    WHEN s.text = '/'
                        THEN paths.full_path || '/'
                    ELSE paths.full_path || '/' || s.text
                END
            FROM directories d
            JOIN paths ON d.parent_id = paths.id
            JOIN strings s ON d.name_id = s.id
        )
        SELECT id, full_path
        FROM paths;
        "#,
    )?;

    Ok(())
}

/// Create query indexes.
/// 创建查询索引。
fn create_indices(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_strings_text ON strings(text);
        CREATE INDEX IF NOT EXISTS idx_directories_parent ON directories(parent_id);

        CREATE INDEX IF NOT EXISTS idx_files_filename_id ON files(filename_id);
        CREATE INDEX IF NOT EXISTS idx_files_dir_id ON files(directory_id);
        CREATE INDEX IF NOT EXISTS idx_files_module_id ON files(module_id);

        CREATE INDEX IF NOT EXISTS idx_classes_covering
            ON classes(name_id, file_id, line_number, symbol_type);
        CREATE INDEX IF NOT EXISTS idx_classes_file_id ON classes(file_id);

        CREATE INDEX IF NOT EXISTS idx_members_name_id ON members(name_id);
        CREATE INDEX IF NOT EXISTS idx_members_file_id ON members(file_id);
        CREATE INDEX IF NOT EXISTS idx_members_class_id ON members(class_id);

        CREATE INDEX IF NOT EXISTS idx_symbol_calls_file_id ON symbol_calls(file_id);
        CREATE INDEX IF NOT EXISTS idx_symbol_calls_name_id ON symbol_calls(name_id);

        CREATE INDEX IF NOT EXISTS idx_file_includes_file_id ON file_includes(file_id);
        CREATE INDEX IF NOT EXISTS idx_file_includes_resolved_id ON file_includes(resolved_file_id);
        CREATE INDEX IF NOT EXISTS idx_file_includes_base_name ON file_includes(base_filename_id);

        CREATE INDEX IF NOT EXISTS idx_cache_last_used ON persistent_cache(last_used);
        CREATE INDEX IF NOT EXISTS idx_assets_asset_key ON assets(asset_key);
        CREATE INDEX IF NOT EXISTS idx_assets_source_path_key ON assets(source_path_key);
        CREATE INDEX IF NOT EXISTS idx_assets_parent_class_key ON assets(parent_class_key);
        CREATE INDEX IF NOT EXISTS idx_asset_references_asset_path ON asset_references(asset_path);
        CREATE INDEX IF NOT EXISTS idx_asset_references_reference_key ON asset_references(reference_key);
        CREATE INDEX IF NOT EXISTS idx_asset_functions_asset_path ON asset_functions(asset_path);
        CREATE INDEX IF NOT EXISTS idx_asset_functions_function_key ON asset_functions(function_key);
        CREATE INDEX IF NOT EXISTS idx_gameplay_tags_identifier ON gameplay_tags(identifier);
        CREATE INDEX IF NOT EXISTS idx_gameplay_tags_tag_path ON gameplay_tags(tag_path);
        CREATE INDEX IF NOT EXISTS idx_gameplay_tags_file_id ON gameplay_tags(file_id);
        CREATE INDEX IF NOT EXISTS idx_macro_definitions_name ON macro_definitions(name);
        CREATE INDEX IF NOT EXISTS idx_macro_definitions_file_id ON macro_definitions(file_id);

        CREATE INDEX IF NOT EXISTS idx_search_files_basename_lc ON search_files(basename_lc);
        CREATE INDEX IF NOT EXISTS idx_search_files_path_lc ON search_files(path_lc);
        CREATE INDEX IF NOT EXISTS idx_search_files_module_name_lc ON search_files(module_name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_files_flags ON search_files(is_source, is_header, is_searchable_text);

        CREATE INDEX IF NOT EXISTS idx_search_symbols_name_lc ON search_symbols(name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_compact_name ON search_symbols(compact_name);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_owner_name_lc ON search_symbols(owner_name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_module_name_lc ON search_symbols(module_name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_basename_lc ON search_symbols(basename_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_kind_rank_name ON search_symbols(kind_rank, name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbols_file_id ON search_symbols(file_id);

        CREATE INDEX IF NOT EXISTS idx_search_text_lines_file_line ON search_text_lines(file_id, line_number);
        CREATE INDEX IF NOT EXISTS idx_search_text_lines_module_name_lc ON search_text_lines(module_name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_text_lines_path_lc ON search_text_lines(path_lc);

        CREATE INDEX IF NOT EXISTS idx_search_symbol_calls_name_lc ON search_symbol_calls(name_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbol_calls_file_id ON search_symbol_calls(file_id);
        CREATE INDEX IF NOT EXISTS idx_search_symbol_calls_path_lc ON search_symbol_calls(path_lc);
        CREATE INDEX IF NOT EXISTS idx_search_symbol_calls_module_name_lc ON search_symbol_calls(module_name_lc);

        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_lookup_kind ON search_asset_usages(lookup_key, usage_kind);
        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_path ON search_asset_usages(asset_path);
        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_name_lc ON search_asset_usages(asset_name_lc);
        "#,
    )?;

    Ok(())
}

/// Drop indexes before large insert batches.
/// 大批量插入前删除索引以提升写入速度。
fn drop_indices(conn: &Connection) -> rusqlite::Result<()> {
    let indices = [
        "idx_strings_text",
        "idx_directories_parent",
        "idx_files_filename_id",
        "idx_files_dir_id",
        "idx_files_module_id",
        "idx_classes_covering",
        "idx_classes_file_id",
        "idx_members_name_id",
        "idx_members_file_id",
        "idx_members_class_id",
        "idx_symbol_calls_file_id",
        "idx_symbol_calls_name_id",
        "idx_file_includes_file_id",
        "idx_file_includes_resolved_id",
        "idx_file_includes_base_name",
        "idx_cache_last_used",
        "idx_assets_asset_key",
        "idx_assets_source_path_key",
        "idx_assets_parent_class_key",
        "idx_asset_references_asset_path",
        "idx_asset_references_reference_key",
        "idx_asset_functions_asset_path",
        "idx_asset_functions_function_key",
        "idx_gameplay_tags_identifier",
        "idx_gameplay_tags_tag_path",
        "idx_gameplay_tags_file_id",
        "idx_macro_definitions_name",
        "idx_macro_definitions_file_id",
        "idx_search_files_basename_lc",
        "idx_search_files_path_lc",
        "idx_search_files_module_name_lc",
        "idx_search_files_flags",
        "idx_search_symbols_name_lc",
        "idx_search_symbols_compact_name",
        "idx_search_symbols_owner_name_lc",
        "idx_search_symbols_module_name_lc",
        "idx_search_symbols_basename_lc",
        "idx_search_symbols_kind_rank_name",
        "idx_search_symbols_file_id",
        "idx_search_text_lines_file_line",
        "idx_search_text_lines_module_name_lc",
        "idx_search_text_lines_path_lc",
        "idx_search_symbol_calls_name_lc",
        "idx_search_symbol_calls_file_id",
        "idx_search_symbol_calls_path_lc",
        "idx_search_symbol_calls_module_name_lc",
        "idx_search_asset_usages_lookup_kind",
        "idx_search_asset_usages_asset_path",
        "idx_search_asset_usages_asset_name_lc",
    ];

    for index_name in indices {
        let sql = format!("DROP INDEX IF EXISTS {}", index_name);
        let _ = conn.execute(&sql, []);
    }

    Ok(())
}

/// Get or create one interned string id.
/// 获取或创建字符串池中的字符串 id。
pub fn get_or_create_string(
    tx: &rusqlite::Transaction,
    cache: &mut HashMap<String, i64>,
    text: &str,
) -> rusqlite::Result<i64> {
    let text = text.trim();

    if let Some(&id) = cache.get(text) {
        return Ok(id);
    }

    let existing = tx
        .query_row(
            "SELECT id FROM strings WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .optional()?;

    let id = match existing {
        Some(id) => id,
        None => {
            tx.execute("INSERT INTO strings (text) VALUES (?1)", [text])?;
            tx.last_insert_rowid()
        }
    };

    cache.insert(text.to_string(), id);
    Ok(id)
}

/// Save parser results into SQLite.
/// 把解析结果保存到 SQLite。
pub fn save_to_db(
    conn: &mut Connection,
    results: &[ParseResult],
    reporter: Arc<dyn ProgressReporter>,
) -> anyhow::Result<()> {
    init_db(conn)?;
    let total = results.len();
    let started_at = Instant::now();

    info!("DB write start: {} parse results", total);

    prepare_bulk_write(conn)?;
    reporter.report("db_write", 0, total.max(1), "Prepare");
    reporter.report("db_write", 0, total.max(1), "Drop");
    drop_indices(conn)?;
    info!("DB write drop indices finished in {} ms", started_at.elapsed().as_millis());

    reporter.report("db_write", 0, total.max(1), "Insert");

    let tx = conn.transaction()?;
    let mut string_cache: HashMap<String, i64> = HashMap::new();
    let mut dir_cache: HashMap<(Option<i64>, i64), i64> = HashMap::new();
    let mut module_name_cache: HashMap<Option<i64>, Option<String>> = HashMap::new();

    {
        let mut stmt_select_file =
            tx.prepare("SELECT id FROM files WHERE directory_id = ?1 AND filename_id = ?2")?;
        let mut stmt_delete_file =
            tx.prepare("DELETE FROM files WHERE directory_id = ?1 AND filename_id = ?2")?;

        let mut stmt_file = tx.prepare(
            "INSERT INTO files
             (directory_id, filename_id, extension, mtime, file_hash, module_id, is_header)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;

        let mut stmt_touch_file = tx.prepare(
            "UPDATE files
             SET extension = ?3, mtime = ?4, module_id = ?5, is_header = ?6
             WHERE directory_id = ?1 AND filename_id = ?2",
        )?;

        let mut stmt_class = tx.prepare(
            "INSERT INTO classes
             (name_id, namespace_id, base_class_id, file_id, line_number, symbol_type, end_line_number)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;

        let mut stmt_inheritance = tx.prepare(
            "INSERT INTO inheritance (child_id, parent_name_id)
             VALUES (?1, ?2)",
        )?;

        let mut stmt_enum = tx.prepare(
            "INSERT INTO enum_values (enum_id, name_id, line_number, file_id)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        let mut stmt_member = tx.prepare(
            "INSERT INTO members
             (class_id, name_id, type_id, flags, access, detail, return_type_id, is_static, line_number, file_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;

        let mut stmt_call = tx.prepare(
            "INSERT INTO symbol_calls (file_id, line, name_id)
             VALUES (?1, ?2, ?3)",
        )?;

        let mut stmt_fts = tx.prepare(
            "INSERT INTO symbols_fts (name, type, class_name, rowid_ref)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        let mut stmt_include = tx.prepare(
            "INSERT INTO file_includes (file_id, include_path_id, base_filename_id)
             VALUES (?1, ?2, ?3)",
        )?;

        let mut stmt_tag = tx.prepare(
            "INSERT INTO gameplay_tags (identifier, tag_path, kind, line_number, file_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;

        let mut stmt_macro = tx.prepare(
            "INSERT INTO macro_definitions (name, line_number, file_id)
             VALUES (?1, ?2, ?3)",
        )?;

        let mut stmt_search_file = tx.prepare(
            "INSERT OR REPLACE INTO search_files
             (file_id, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, is_source, is_header, is_searchable_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;

        let mut stmt_search_symbol = tx.prepare(
            "INSERT INTO search_symbols
             (file_id, symbol_rowid, symbol_table, kind, kind_rank, name, name_lc, compact_name, owner_name, owner_name_lc, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, line_number, is_class_like, is_function_like, is_member_like)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        )?;

        let mut stmt_update_search_symbol_meta = tx.prepare(
            "UPDATE search_symbols
             SET module_id = ?1,
                 module_name = ?2,
                 module_name_lc = ?3,
                 path = ?4,
                 path_lc = ?5,
                 basename = ?6,
                 basename_lc = ?7,
                 ext = ?8
             WHERE file_id = ?9",
        )?;

        let mut stmt_delete_search_text = tx.prepare(
            "DELETE FROM search_text_lines WHERE file_id = ?1",
        )?;

        let mut stmt_insert_search_text = tx.prepare(
            "INSERT INTO search_text_lines
             (file_id, module_id, module_name, module_name_lc, path, path_lc, line_number, line_text, line_text_lc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        let mut stmt_update_search_text_meta = tx.prepare(
            "UPDATE search_text_lines
             SET module_id = ?1,
                 module_name = ?2,
                 module_name_lc = ?3,
                 path = ?4,
                 path_lc = ?5
             WHERE file_id = ?6",
        )?;

        let mut stmt_search_call = tx.prepare(
            "INSERT INTO search_symbol_calls
             (file_id, module_id, module_name, module_name_lc, path, path_lc, line_number, name, name_lc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        let mut stmt_update_search_call_meta = tx.prepare(
            "UPDATE search_symbol_calls
             SET module_id = ?1,
                 module_name = ?2,
                 module_name_lc = ?3,
                 path = ?4,
                 path_lc = ?5
             WHERE file_id = ?6",
        )?;

        let mut last_reported_percent = 0usize;

        for (index, result) in results.iter().enumerate() {
            let current = index + 1;
            let percent = progress_percent(current, total);

            if current == total || current == 1 || current % ITEM_PROGRESS_EVERY == 0 || percent > last_reported_percent {
                last_reported_percent = percent;
                reporter.report(
                    "db_write",
                    current,
                    total,
                    &format!("{}/{}", current, total),
                );
            }

            let path_obj = Path::new(&result.path);
            let parent_dir = path_obj.parent().unwrap_or_else(|| Path::new(""));
            let filename = path_obj
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            let extension = path_obj
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            let dir_id = project_path::get_or_create_directory(
                &tx,
                &mut string_cache,
                &mut dir_cache,
                parent_dir,
            )?;
            let filename_id = get_or_create_string(&tx, &mut string_cache, filename)?;
            let existing_file_id = stmt_select_file
                .query_row(params![dir_id, filename_id], |row| row.get::<_, i64>(0))
                .optional()?;

            if result.status == "cache_hit" {
                stmt_touch_file.execute(params![
                    dir_id,
                    filename_id,
                    extension,
                    result.mtime as i64,
                    result.module_id,
                    is_header_extension(&extension) as i32,
                ])?;

                if let Some(file_id) = existing_file_id {
                    upsert_search_file_row_tx(
                        &tx,
                        &mut module_name_cache,
                        &mut stmt_search_file,
                        file_id,
                        result.module_id,
                        path_obj,
                        &extension,
                    )?;

                    let module_name =
                        module_name_for_id_tx(&tx, &mut module_name_cache, result.module_id)?;
                    let module_name_lc = module_name.as_deref().map(lower_ascii);
                    let path = normalize_indexed_path(path_obj);
                    let path_lc = lower_ascii(&path);
                    let basename = filename.to_string();
                    let basename_lc = lower_ascii(filename);

                    stmt_update_search_symbol_meta.execute(params![
                        result.module_id,
                        module_name.as_deref(),
                        module_name_lc.as_deref(),
                        &path,
                        &path_lc,
                        &basename,
                        &basename_lc,
                        &extension,
                        file_id,
                    ])?;

                    stmt_update_search_text_meta.execute(params![
                        result.module_id,
                        module_name.as_deref(),
                        module_name_lc.as_deref(),
                        &path,
                        &path_lc,
                        file_id,
                    ])?;

                    stmt_update_search_call_meta.execute(params![
                        result.module_id,
                        module_name.as_deref(),
                        module_name_lc.as_deref(),
                        &path,
                        &path_lc,
                        file_id,
                    ])?;
                }
                continue;
            }

            if result.status != "parsed" {
                continue;
            }

            let Some(data) = &result.data else {
                continue;
            };

            let _ = stmt_delete_file.execute(params![dir_id, filename_id]);

            stmt_file.execute(params![
                dir_id,
                filename_id,
                extension,
                result.mtime as i64,
                data.new_hash,
                result.module_id,
                is_header_extension(&extension) as i32,
            ])?;

            let file_id = tx.last_insert_rowid();
            upsert_search_file_row_tx(
                &tx,
                &mut module_name_cache,
                &mut stmt_search_file,
                file_id,
                result.module_id,
                path_obj,
                &extension,
            )?;
            let module_name =
                module_name_for_id_tx(&tx, &mut module_name_cache, result.module_id)?;

            save_classes(
                &tx,
                &mut string_cache,
                &mut stmt_class,
                &mut stmt_inheritance,
                &mut stmt_enum,
                &mut stmt_member,
                &mut stmt_fts,
                &mut stmt_search_symbol,
                file_id,
                result.module_id,
                module_name.as_deref(),
                path_obj,
                &extension,
                &data.classes,
            )?;

            save_calls(
                &tx,
                &mut string_cache,
                &mut stmt_call,
                &mut stmt_search_call,
                file_id,
                result.module_id,
                module_name.as_deref(),
                path_obj,
                &data.calls,
            )?;

            save_includes(
                &tx,
                &mut string_cache,
                &mut stmt_include,
                file_id,
                &data.includes,
            )?;

            save_gameplay_tags(&mut stmt_tag, file_id, &data.gameplay_tags)?;
            save_macro_definitions(&mut stmt_macro, file_id, &data.macro_definitions)?;
            replace_search_text_lines_for_file_tx(
                &tx,
                &mut module_name_cache,
                &mut stmt_delete_search_text,
                &mut stmt_insert_search_text,
                file_id,
                result.module_id,
                path_obj,
                &extension,
            )?;
        }
    }

    reporter.report("db_write", total.max(1), total.max(1), "Commit");
    let commit_started_at = Instant::now();
    tx.commit()?;
    info!(
        "DB write commit finished in {} ms (total {} ms)",
        commit_started_at.elapsed().as_millis(),
        started_at.elapsed().as_millis()
    );

    finalize_bulk_write(conn, reporter)?;
    Ok(())
}

pub fn ensure_search_projections(conn: &Connection) -> anyhow::Result<()> {
    let search_file_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM search_files", [], |row| row.get(0))?;
    let search_symbol_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM search_symbols", [], |row| row.get(0))?;
    let search_call_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM search_symbol_calls", [], |row| row.get(0))?;

    if search_file_count > 0 && search_symbol_count > 0 && search_call_count > 0 {
        return Ok(());
    }

    if search_file_count == 0 {
        let sql = format!(
            r#"
            {}
            INSERT INTO search_files
                (file_id, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, is_source, is_header, is_searchable_text)
            SELECT
                f.id,
                f.module_id,
                sm.text,
                lower(sm.text),
                CASE
                    WHEN dp.full_path = '/' THEN '/' || sn.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sn.text
                    ELSE dp.full_path || '/' || sn.text
                END,
                lower(CASE
                    WHEN dp.full_path = '/' THEN '/' || sn.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sn.text
                    ELSE dp.full_path || '/' || sn.text
                END),
                sn.text,
                lower(sn.text),
                f.extension,
                CASE WHEN lower(COALESCE(f.extension, '')) IN ('h','hh','hpp','hxx','inl','ipp','c','cc','cpp','cxx','cs') THEN 1 ELSE 0 END,
                COALESCE(f.is_header, 0),
                CASE WHEN lower(COALESCE(f.extension, '')) IN ('h','hh','hpp','hxx','c','cc','cpp','cxx','inl','ipp','cs','ini','json','uproject','uplugin') THEN 1 ELSE 0 END
            FROM files f
            JOIN dir_paths dp ON f.directory_id = dp.id
            JOIN strings sn ON f.filename_id = sn.id
            LEFT JOIN modules m ON f.module_id = m.id
            LEFT JOIN strings sm ON m.name_id = sm.id
            "#,
            project_path::PATH_CTE
        );
        conn.execute_batch(&sql)?;
    }

    if search_symbol_count == 0 {
        let class_sql = format!(
            r#"
            {}
            INSERT INTO search_symbols
                (file_id, symbol_rowid, symbol_table, kind, kind_rank, name, name_lc, compact_name, owner_name, owner_name_lc, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, line_number, is_class_like, is_function_like, is_member_like)
            SELECT
                f.id,
                c.id,
                'classes',
                c.symbol_type,
                CASE
                    WHEN lower(COALESCE(c.symbol_type, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') THEN 0
                    WHEN lower(COALESCE(c.symbol_type, '')) LIKE '%function%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%method%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%delegate%' THEN 1
                    ELSE 2
                END,
                sc.text,
                lower(sc.text),
                lower(replace(replace(replace(sc.text, '_', ''), ':', ''), ' ', '')),
                sc.text,
                lower(sc.text),
                f.module_id,
                sm.text,
                lower(sm.text),
                CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END,
                lower(CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END),
                sf.text,
                lower(sf.text),
                f.extension,
                c.line_number,
                CASE WHEN lower(COALESCE(c.symbol_type, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') THEN 1 ELSE 0 END,
                CASE WHEN lower(COALESCE(c.symbol_type, '')) LIKE '%function%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%method%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%delegate%' THEN 1 ELSE 0 END,
                CASE WHEN lower(COALESCE(c.symbol_type, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') OR lower(COALESCE(c.symbol_type, '')) LIKE '%function%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%method%' OR lower(COALESCE(c.symbol_type, '')) LIKE '%delegate%' THEN 0 ELSE 1 END
            FROM classes c
            JOIN strings sc ON c.name_id = sc.id
            JOIN files f ON c.file_id = f.id
            JOIN dir_paths dp ON f.directory_id = dp.id
            JOIN strings sf ON f.filename_id = sf.id
            LEFT JOIN modules m ON f.module_id = m.id
            LEFT JOIN strings sm ON m.name_id = sm.id;

            INSERT INTO search_symbols
                (file_id, symbol_rowid, symbol_table, kind, kind_rank, name, name_lc, compact_name, owner_name, owner_name_lc, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, line_number, is_class_like, is_function_like, is_member_like)
            SELECT
                COALESCE(m.file_id, c.file_id),
                m.id,
                'members',
                st.text,
                CASE
                    WHEN lower(COALESCE(st.text, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') THEN 0
                    WHEN lower(COALESCE(st.text, '')) LIKE '%function%' OR lower(COALESCE(st.text, '')) LIKE '%method%' OR lower(COALESCE(st.text, '')) LIKE '%delegate%' THEN 1
                    ELSE 2
                END,
                sn.text,
                lower(sn.text),
                lower(replace(replace(replace(sn.text, '_', ''), ':', ''), ' ', '')),
                sc.text,
                lower(sc.text),
                f.module_id,
                sm.text,
                lower(sm.text),
                CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END,
                lower(CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END),
                sf.text,
                lower(sf.text),
                f.extension,
                m.line_number,
                CASE WHEN lower(COALESCE(st.text, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') THEN 1 ELSE 0 END,
                CASE WHEN lower(COALESCE(st.text, '')) LIKE '%function%' OR lower(COALESCE(st.text, '')) LIKE '%method%' OR lower(COALESCE(st.text, '')) LIKE '%delegate%' THEN 1 ELSE 0 END,
                CASE WHEN lower(COALESCE(st.text, '')) IN ('class','struct','enum','uclass','ustruct','uenum','uinterface','typedef') OR lower(COALESCE(st.text, '')) LIKE '%function%' OR lower(COALESCE(st.text, '')) LIKE '%method%' OR lower(COALESCE(st.text, '')) LIKE '%delegate%' THEN 0 ELSE 1 END
            FROM members m
            JOIN strings sn ON m.name_id = sn.id
            JOIN strings st ON m.type_id = st.id
            JOIN classes c ON m.class_id = c.id
            JOIN strings sc ON c.name_id = sc.id
            JOIN files f ON COALESCE(m.file_id, c.file_id) = f.id
            JOIN dir_paths dp ON f.directory_id = dp.id
            JOIN strings sf ON f.filename_id = sf.id
            LEFT JOIN modules mo ON f.module_id = mo.id
            LEFT JOIN strings sm ON mo.name_id = sm.id
            WHERE COALESCE(m.file_id, c.file_id) IS NOT NULL
            "#,
            project_path::PATH_CTE
        );
        conn.execute_batch(&class_sql)?;
    }

    if search_call_count == 0 {
        let sql = format!(
            r#"
            {}
            INSERT INTO search_symbol_calls
                (file_id, module_id, module_name, module_name_lc, path, path_lc, line_number, name, name_lc)
            SELECT
                sc.file_id,
                f.module_id,
                sm.text,
                lower(sm.text),
                CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END,
                lower(CASE
                    WHEN dp.full_path = '/' THEN '/' || sf.text
                    WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sf.text
                    ELSE dp.full_path || '/' || sf.text
                END),
                sc.line,
                ss.text,
                lower(ss.text)
            FROM symbol_calls sc
            JOIN strings ss ON sc.name_id = ss.id
            JOIN files f ON sc.file_id = f.id
            JOIN dir_paths dp ON f.directory_id = dp.id
            JOIN strings sf ON f.filename_id = sf.id
            LEFT JOIN modules m ON f.module_id = m.id
            LEFT JOIN strings sm ON m.name_id = sm.id
            "#,
            project_path::PATH_CTE
        );
        conn.execute_batch(&sql)?;
    }

    Ok(())
}

/// Save parser results using incremental writes without global index rebuild.
/// 使用增量写入方式保存解析结果，避免全局索引重建。
pub fn save_to_db_incremental(
    conn: &mut Connection,
    results: &[ParseResult],
    reporter: Arc<dyn ProgressReporter>,
) -> anyhow::Result<()> {
    init_db(conn)?;

    if results.is_empty() {
        return Ok(());
    }

    let total = results.len();
    let started_at = Instant::now();
    let tx = conn.transaction()?;
    let mut string_cache: HashMap<String, i64> = HashMap::new();
    let mut dir_cache: HashMap<(Option<i64>, i64), i64> = HashMap::new();
    let mut module_name_cache: HashMap<Option<i64>, Option<String>> = HashMap::new();

    struct PlannedWrite<'a> {
        result: &'a ParseResult,
        dir_id: i64,
        filename_id: i64,
        extension: String,
        existing_file_id: Option<i64>,
    }

    let mut planned = Vec::with_capacity(results.len());
    let mut stmt_select_file =
        tx.prepare("SELECT id FROM files WHERE directory_id = ?1 AND filename_id = ?2")?;

    for result in results {
        let path_obj = Path::new(&result.path);
        let parent_dir = path_obj.parent().unwrap_or_else(|| Path::new(""));
        let filename = path_obj
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let extension = path_obj
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let dir_id = project_path::get_or_create_directory(
            &tx,
            &mut string_cache,
            &mut dir_cache,
            parent_dir,
        )?;
        let filename_id = get_or_create_string(&tx, &mut string_cache, filename)?;
        let existing_file_id = stmt_select_file
            .query_row(params![dir_id, filename_id], |row| row.get::<_, i64>(0))
            .optional()?;

        planned.push(PlannedWrite {
            result,
            dir_id,
            filename_id,
            extension,
            existing_file_id,
        });
    }

    drop(stmt_select_file);

    let replaced_file_ids = planned
        .iter()
        .filter_map(|item| {
            if item.result.status == "parsed" && item.result.data.is_some() {
                item.existing_file_id
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let mut affected_class_name_ids = collect_class_name_ids_for_files_tx(&tx, &replaced_file_ids)?;
    let mut affected_filename_ids = collect_filename_ids_for_files_tx(&tx, &replaced_file_ids)?;

    delete_files_by_ids_tx(&tx, &replaced_file_ids)?;

    let mut stmt_insert_file = tx.prepare(
        "INSERT INTO files
         (directory_id, filename_id, extension, mtime, file_hash, module_id, is_header)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;

    let mut stmt_touch_file = tx.prepare(
        "UPDATE files
         SET extension = ?2, mtime = ?3, module_id = ?4, is_header = ?5
         WHERE id = ?1",
    )?;

    let mut stmt_class = tx.prepare(
        "INSERT INTO classes
         (name_id, namespace_id, base_class_id, file_id, line_number, symbol_type, end_line_number)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;

    let mut stmt_inheritance = tx.prepare(
        "INSERT INTO inheritance (child_id, parent_name_id)
         VALUES (?1, ?2)",
    )?;

    let mut stmt_enum = tx.prepare(
        "INSERT INTO enum_values (enum_id, name_id, line_number, file_id)
         VALUES (?1, ?2, ?3, ?4)",
    )?;

    let mut stmt_member = tx.prepare(
        "INSERT INTO members
         (class_id, name_id, type_id, flags, access, detail, return_type_id, is_static, line_number, file_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;

    let mut stmt_call = tx.prepare(
        "INSERT INTO symbol_calls (file_id, line, name_id)
         VALUES (?1, ?2, ?3)",
    )?;

    let mut stmt_fts = tx.prepare(
        "INSERT INTO symbols_fts (name, type, class_name, rowid_ref)
         VALUES (?1, ?2, ?3, ?4)",
    )?;

    let mut stmt_include = tx.prepare(
        "INSERT INTO file_includes (file_id, include_path_id, base_filename_id)
         VALUES (?1, ?2, ?3)",
    )?;

    let mut stmt_tag = tx.prepare(
        "INSERT INTO gameplay_tags (identifier, tag_path, kind, line_number, file_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;

    let mut stmt_macro = tx.prepare(
        "INSERT INTO macro_definitions (name, line_number, file_id)
         VALUES (?1, ?2, ?3)",
    )?;

    let mut stmt_search_file = tx.prepare(
        "INSERT OR REPLACE INTO search_files
         (file_id, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, is_source, is_header, is_searchable_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;

    let mut stmt_search_symbol = tx.prepare(
        "INSERT INTO search_symbols
         (file_id, symbol_rowid, symbol_table, kind, kind_rank, name, name_lc, compact_name, owner_name, owner_name_lc, module_id, module_name, module_name_lc, path, path_lc, basename, basename_lc, ext, line_number, is_class_like, is_function_like, is_member_like)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
    )?;

    let mut stmt_update_search_symbol_meta = tx.prepare(
        "UPDATE search_symbols
         SET module_id = ?1,
             module_name = ?2,
             module_name_lc = ?3,
             path = ?4,
             path_lc = ?5,
             basename = ?6,
             basename_lc = ?7,
             ext = ?8
         WHERE file_id = ?9",
    )?;

    let mut stmt_delete_search_text = tx.prepare(
        "DELETE FROM search_text_lines WHERE file_id = ?1",
    )?;

    let mut stmt_insert_search_text = tx.prepare(
        "INSERT INTO search_text_lines
         (file_id, module_id, module_name, module_name_lc, path, path_lc, line_number, line_text, line_text_lc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;

    let mut stmt_update_search_text_meta = tx.prepare(
        "UPDATE search_text_lines
         SET module_id = ?1,
             module_name = ?2,
             module_name_lc = ?3,
             path = ?4,
             path_lc = ?5
         WHERE file_id = ?6",
    )?;

    let mut stmt_search_call = tx.prepare(
        "INSERT INTO search_symbol_calls
         (file_id, module_id, module_name, module_name_lc, path, path_lc, line_number, name, name_lc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;

    let mut stmt_update_search_call_meta = tx.prepare(
        "UPDATE search_symbol_calls
         SET module_id = ?1,
             module_name = ?2,
             module_name_lc = ?3,
             path = ?4,
             path_lc = ?5
         WHERE file_id = ?6",
    )?;

    let mut last_reported_percent = 0usize;
    let mut written_file_ids = Vec::new();

    for (index, item) in planned.iter().enumerate() {
        let current = index + 1;
        let percent = progress_percent(current, total);

        if current == total || current == 1 || current % ITEM_PROGRESS_EVERY == 0 || percent > last_reported_percent {
            last_reported_percent = percent;
            reporter.report("db_write", current, total, &format!("{}/{}", current, total));
        }

        affected_filename_ids.insert(item.filename_id);

        if item.result.status == "cache_hit" {
            if let Some(existing_file_id) = item.existing_file_id {
                stmt_touch_file.execute(params![
                    existing_file_id,
                    item.extension,
                    item.result.mtime as i64,
                    item.result.module_id,
                    is_header_extension(&item.extension) as i32,
                ])?;

                upsert_search_file_row_tx(
                    &tx,
                    &mut module_name_cache,
                    &mut stmt_search_file,
                    existing_file_id,
                    item.result.module_id,
                    Path::new(&item.result.path),
                    &item.extension,
                )?;

                let module_name =
                    module_name_for_id_tx(&tx, &mut module_name_cache, item.result.module_id)?;
                let module_name_lc = module_name.as_deref().map(lower_ascii);
                let path = normalize_indexed_path(Path::new(&item.result.path));
                let path_lc = lower_ascii(&path);
                let basename = Path::new(&item.result.path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_string();
                let basename_lc = lower_ascii(&basename);

                stmt_update_search_symbol_meta.execute(params![
                    item.result.module_id,
                    module_name.as_deref(),
                    module_name_lc.as_deref(),
                    &path,
                    &path_lc,
                    &basename,
                    &basename_lc,
                    &item.extension,
                    existing_file_id,
                ])?;

                stmt_update_search_text_meta.execute(params![
                    item.result.module_id,
                    module_name.as_deref(),
                    module_name_lc.as_deref(),
                    &path,
                    &path_lc,
                    existing_file_id,
                ])?;

                stmt_update_search_call_meta.execute(params![
                    item.result.module_id,
                    module_name.as_deref(),
                    module_name_lc.as_deref(),
                    &path,
                    &path_lc,
                    existing_file_id,
                ])?;
            }
            continue;
        }

        if item.result.status != "parsed" {
            continue;
        }

        let Some(data) = &item.result.data else {
            continue;
        };

        for class_info in &data.classes {
            let class_name_id = get_or_create_string(&tx, &mut string_cache, &class_info.class_name)?;
            affected_class_name_ids.insert(class_name_id);
        }

        stmt_insert_file.execute(params![
            item.dir_id,
            item.filename_id,
            item.extension,
            item.result.mtime as i64,
            data.new_hash,
            item.result.module_id,
            is_header_extension(&item.extension) as i32,
        ])?;

        let file_id = tx.last_insert_rowid();
        written_file_ids.push(file_id);
        upsert_search_file_row_tx(
            &tx,
            &mut module_name_cache,
            &mut stmt_search_file,
            file_id,
            item.result.module_id,
            Path::new(&item.result.path),
            &item.extension,
        )?;
        let module_name =
            module_name_for_id_tx(&tx, &mut module_name_cache, item.result.module_id)?;

        save_classes(
            &tx,
            &mut string_cache,
            &mut stmt_class,
            &mut stmt_inheritance,
            &mut stmt_enum,
            &mut stmt_member,
            &mut stmt_fts,
            &mut stmt_search_symbol,
            file_id,
            item.result.module_id,
            module_name.as_deref(),
            Path::new(&item.result.path),
            &item.extension,
            &data.classes,
        )?;

        save_calls(
            &tx,
            &mut string_cache,
            &mut stmt_call,
            &mut stmt_search_call,
            file_id,
            item.result.module_id,
            module_name.as_deref(),
            Path::new(&item.result.path),
            &data.calls,
        )?;

        save_includes(
            &tx,
            &mut string_cache,
            &mut stmt_include,
            file_id,
            &data.includes,
        )?;

        save_gameplay_tags(&mut stmt_tag, file_id, &data.gameplay_tags)?;
        save_macro_definitions(&mut stmt_macro, file_id, &data.macro_definitions)?;
        replace_search_text_lines_for_file_tx(
            &tx,
            &mut module_name_cache,
            &mut stmt_delete_search_text,
            &mut stmt_insert_search_text,
            file_id,
            item.result.module_id,
            Path::new(&item.result.path),
            &item.extension,
        )?;
    }

    drop(stmt_update_search_text_meta);
    drop(stmt_insert_search_text);
    drop(stmt_delete_search_text);
    drop(stmt_update_search_call_meta);
    drop(stmt_search_call);
    drop(stmt_update_search_symbol_meta);
    drop(stmt_search_symbol);
    drop(stmt_search_file);
    drop(stmt_macro);
    drop(stmt_tag);
    drop(stmt_include);
    drop(stmt_fts);
    drop(stmt_call);
    drop(stmt_member);
    drop(stmt_enum);
    drop(stmt_inheritance);
    drop(stmt_class);
    drop(stmt_touch_file);
    drop(stmt_insert_file);

    resolve_inheritance_incremental_tx(
        &tx,
        &written_file_ids,
        &affected_class_name_ids.into_iter().collect::<Vec<_>>(),
    )?;
    resolve_file_includes_incremental_tx(
        &tx,
        &written_file_ids,
        &affected_filename_ids.into_iter().collect::<Vec<_>>(),
    )?;

    reporter.report("db_write", total.max(1), total.max(1), "Commit");
    tx.commit()?;
    info!(
        "DB incremental write finished in {} ms ({} results)",
        started_at.elapsed().as_millis(),
        total
    );

    Ok(())
}

/// Delete file rows and all symbol side tables by file id.
/// 按 file id 删除文件及其关联的符号数据。
pub fn delete_files_by_ids(conn: &mut Connection, file_ids: &[i64]) -> anyhow::Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }

    init_db(conn)?;
    let tx = conn.transaction()?;
    delete_files_by_ids_tx(&tx, file_ids)?;
    tx.commit()?;
    Ok(())
}

fn lower_ascii(input: &str) -> String {
    input.to_ascii_lowercase()
}

fn compact_identifier_for_search(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn normalize_indexed_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_source_extension(extension: &str) -> bool {
    matches!(extension, "h" | "hh" | "hpp" | "hxx" | "inl" | "ipp" | "c" | "cc" | "cpp" | "cxx" | "cs")
}

fn is_search_text_extension(extension: &str) -> bool {
    matches!(
        extension,
        "h" | "hh" | "hpp" | "hxx"
            | "c" | "cc" | "cpp" | "cxx"
            | "inl" | "ipp"
            | "cs" | "ini" | "json" | "uproject" | "uplugin"
    )
}

fn search_symbol_kind_rank(kind: &str) -> i64 {
    match kind.to_ascii_lowercase().as_str() {
        "class" | "struct" | "enum" | "uclass" | "ustruct" | "uenum" | "uinterface" | "typedef" => 0,
        "function" | "method" | "delegate" => 1,
        _ => 2,
    }
}

fn is_class_like_search_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "class" | "struct" | "enum" | "uclass" | "ustruct" | "uenum" | "uinterface" | "typedef"
    )
}

fn is_function_like_search_kind(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    kind.contains("function") || kind.contains("method") || kind.contains("delegate")
}

fn is_member_like_search_kind(kind: &str) -> bool {
    !is_class_like_search_kind(kind) && !is_function_like_search_kind(kind)
}

fn module_name_for_id_tx(
    tx: &rusqlite::Transaction,
    cache: &mut HashMap<Option<i64>, Option<String>>,
    module_id: Option<i64>,
) -> anyhow::Result<Option<String>> {
    if let Some(cached) = cache.get(&module_id) {
        return Ok(cached.clone());
    }

    let value = if let Some(module_id) = module_id {
        tx.query_row(
            r#"
            SELECT s.text
            FROM modules m
            JOIN strings s ON m.name_id = s.id
            WHERE m.id = ?1
            "#,
            params![module_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    } else {
        None
    };

    cache.insert(module_id, value.clone());
    Ok(value)
}

pub(crate) fn upsert_search_file_row_tx(
    tx: &rusqlite::Transaction,
    module_name_cache: &mut HashMap<Option<i64>, Option<String>>,
    stmt: &mut rusqlite::Statement,
    file_id: i64,
    module_id: Option<i64>,
    path: &Path,
    extension: &str,
) -> anyhow::Result<()> {
    let path = normalize_indexed_path(path);
    let path_lc = lower_ascii(&path);
    let basename = path
        .rsplit('/')
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    let basename_lc = lower_ascii(&basename);
    let module_name = module_name_for_id_tx(tx, module_name_cache, module_id)?;
    let module_name_lc = module_name.as_deref().map(lower_ascii);

    stmt.execute(params![
        file_id,
        module_id,
        module_name,
        module_name_lc,
        path,
        path_lc,
        basename,
        basename_lc,
        extension,
        is_source_extension(extension) as i32,
        is_header_extension(extension) as i32,
        is_search_text_extension(extension) as i32,
    ])?;

    Ok(())
}

pub(crate) fn replace_search_text_lines_for_file_tx(
    tx: &rusqlite::Transaction,
    module_name_cache: &mut HashMap<Option<i64>, Option<String>>,
    stmt_delete: &mut rusqlite::Statement,
    stmt_insert: &mut rusqlite::Statement,
    file_id: i64,
    module_id: Option<i64>,
    path: &Path,
    extension: &str,
) -> anyhow::Result<()> {
    stmt_delete.execute(params![file_id])?;

    if !is_search_text_extension(extension) {
        return Ok(());
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(());
    };

    let path = normalize_indexed_path(path);
    let path_lc = lower_ascii(&path);
    let module_name = module_name_for_id_tx(tx, module_name_cache, module_id)?;
    let module_name_lc = module_name.as_deref().map(lower_ascii);

    for (line_index, raw_line) in content.lines().enumerate() {
        let line_text = raw_line.trim_end();
        if line_text.trim().is_empty() {
            continue;
        }

        stmt_insert.execute(params![
            file_id,
            module_id,
            module_name,
            module_name_lc,
            path,
            path_lc,
            (line_index + 1) as i64,
            line_text,
            lower_ascii(line_text),
        ])?;
    }

    Ok(())
}

fn insert_search_symbol_row(
    stmt: &mut rusqlite::Statement,
    file_id: i64,
    symbol_rowid: i64,
    symbol_table: &str,
    kind: &str,
    name: &str,
    owner_name: Option<&str>,
    module_id: Option<i64>,
    module_name: Option<&str>,
    path: &Path,
    extension: &str,
    line_number: i64,
) -> anyhow::Result<()> {
    let path = normalize_indexed_path(path);
    let path_lc = lower_ascii(&path);
    let basename = path
        .rsplit('/')
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    let basename_lc = lower_ascii(&basename);
    let module_name_lc = module_name.map(lower_ascii);
    let owner_name_lc = owner_name.map(lower_ascii);

    stmt.execute(params![
        file_id,
        symbol_rowid,
        symbol_table,
        kind,
        search_symbol_kind_rank(kind),
        name,
        lower_ascii(name),
        compact_identifier_for_search(name),
        owner_name,
        owner_name_lc,
        module_id,
        module_name,
        module_name_lc,
        path,
        path_lc,
        basename,
        basename_lc,
        extension,
        line_number,
        is_class_like_search_kind(kind) as i32,
        is_function_like_search_kind(kind) as i32,
        is_member_like_search_kind(kind) as i32,
    ])?;

    Ok(())
}

fn insert_search_symbol_call_row(
    stmt: &mut rusqlite::Statement,
    file_id: i64,
    module_id: Option<i64>,
    module_name: Option<&str>,
    path: &Path,
    line_number: i64,
    name: &str,
) -> anyhow::Result<()> {
    let path = normalize_indexed_path(path);
    let path_lc = lower_ascii(&path);
    let module_name_lc = module_name.map(lower_ascii);

    stmt.execute(params![
        file_id,
        module_id,
        module_name,
        module_name_lc,
        path,
        path_lc,
        line_number,
        name,
        lower_ascii(name),
    ])?;

    Ok(())
}

/// Convert item progress into a 0-100 percentage.
/// 将条目进度换算成 0-100 百分比。
fn progress_percent(current: usize, total: usize) -> usize {
    if total == 0 {
        return 100;
    }

    (current * 100 / total).min(100)
}

/// Configure SQLite for fast bulk insertion.
/// 配置 SQLite 以提升批量写入性能。
fn prepare_bulk_write(conn: &Connection) -> anyhow::Result<()> {
    conn.busy_timeout(DB_BULK_BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.pragma_update(None, "cache_size", "-800000")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.execute("PRAGMA foreign_keys = OFF", [])?;
    Ok(())
}

/// Restore indexes, resolve links, and optimize after bulk insertion.
/// 批量写入后恢复索引、解析关系并优化数据库。
fn finalize_bulk_write(
    conn: &mut Connection,
    reporter: Arc<dyn ProgressReporter>,
) -> anyhow::Result<()> {
    let started_at = Instant::now();
    reporter.report("finalizing", 60, 100, "Commit");
    reporter.report(
        "finalizing",
        70,
        100,
        "Indices",
    );
    create_indices(conn)?;

    conn.execute("PRAGMA foreign_keys = ON", [])?;

    reporter.report("finalizing", 80, 100, "Inheritance");
    resolve_inheritance(conn)?;

    reporter.report("finalizing", 85, 100, "Includes");
    resolve_file_includes_by_path(conn)?;

    reporter.report("finalizing", 95, 100, "Optimize");
    conn.execute("PRAGMA optimize", [])?;
    info!("DB finalize finished in {} ms", started_at.elapsed().as_millis());

    Ok(())
}

fn collect_class_name_ids_for_files_tx(
    tx: &rusqlite::Transaction,
    file_ids: &[i64],
) -> anyhow::Result<HashSet<i64>> {
    let mut ids = HashSet::new();
    let mut stmt = tx.prepare("SELECT DISTINCT name_id FROM classes WHERE file_id = ?1")?;

    for file_id in file_ids {
        let rows = stmt.query_map(params![file_id], |row| row.get::<_, i64>(0))?;
        for row in rows {
            ids.insert(row?);
        }
    }

    Ok(ids)
}

fn collect_filename_ids_for_files_tx(
    tx: &rusqlite::Transaction,
    file_ids: &[i64],
) -> anyhow::Result<HashSet<i64>> {
    let mut ids = HashSet::new();
    let mut stmt = tx.prepare("SELECT filename_id FROM files WHERE id = ?1")?;

    for file_id in file_ids {
        if let Some(filename_id) = stmt
            .query_row(params![file_id], |row| row.get::<_, i64>(0))
            .optional()?
        {
            ids.insert(filename_id);
        }
    }

    Ok(ids)
}

fn populate_temp_ids_table(
    tx: &rusqlite::Transaction,
    table_name: &str,
    ids: &[i64],
) -> anyhow::Result<()> {
    tx.execute_batch(&format!(
        "DROP TABLE IF EXISTS {0}; CREATE TEMP TABLE {0} (id INTEGER PRIMARY KEY);",
        table_name
    ))?;

    let mut stmt = tx.prepare(&format!(
        "INSERT OR IGNORE INTO {} (id) VALUES (?1)",
        table_name
    ))?;

    for id in ids {
        stmt.execute(params![id])?;
    }

    Ok(())
}

pub(crate) fn delete_files_by_ids_tx(
    tx: &rusqlite::Transaction,
    file_ids: &[i64],
) -> anyhow::Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }

    populate_temp_ids_table(tx, "temp_ucore_deleted_file_ids", file_ids)?;

    tx.execute(
        r#"
        DELETE FROM symbols_fts
        WHERE rowid_ref IN (
            SELECT c.id
            FROM classes c
            JOIN temp_ucore_deleted_file_ids t ON t.id = c.file_id
        )
        OR rowid_ref IN (
            SELECT m.id
            FROM members m
            JOIN temp_ucore_deleted_file_ids t ON t.id = m.file_id
        )
        "#,
        [],
    )?;

    tx.execute(
        r#"
        UPDATE inheritance
        SET parent_class_id = NULL
        WHERE parent_class_id IN (
            SELECT c.id
            FROM classes c
            JOIN temp_ucore_deleted_file_ids t ON t.id = c.file_id
        )
        "#,
        [],
    )?;

    tx.execute(
        "UPDATE file_includes
         SET resolved_file_id = NULL
         WHERE resolved_file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        [],
    )?;

    for sql in [
        "DELETE FROM search_text_lines WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM search_symbols WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM search_symbol_calls WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM search_files WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM gameplay_tags WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM macro_definitions WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM symbol_calls WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM file_includes WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM members WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM enum_values WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM inheritance WHERE child_id IN (SELECT c.id FROM classes c JOIN temp_ucore_deleted_file_ids t ON t.id = c.file_id)",
        "DELETE FROM classes WHERE file_id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
        "DELETE FROM files WHERE id IN (SELECT id FROM temp_ucore_deleted_file_ids)",
    ] {
        tx.execute(sql, [])?;
    }

    tx.execute_batch("DROP TABLE IF EXISTS temp_ucore_deleted_file_ids;")?;
    Ok(())
}

fn resolve_inheritance_incremental_tx(
    tx: &rusqlite::Transaction,
    changed_file_ids: &[i64],
    affected_name_ids: &[i64],
) -> anyhow::Result<()> {
    if changed_file_ids.is_empty() && affected_name_ids.is_empty() {
        return Ok(());
    }

    populate_temp_ids_table(tx, "temp_ucore_changed_file_ids", changed_file_ids)?;
    populate_temp_ids_table(tx, "temp_ucore_changed_name_ids", affected_name_ids)?;

    tx.execute(
        r#"
        UPDATE inheritance
        SET parent_class_id = (
            SELECT c.id
            FROM classes c
            WHERE c.name_id = inheritance.parent_name_id
            LIMIT 1
        )
        WHERE child_id IN (
                SELECT c.id
                FROM classes c
                JOIN temp_ucore_changed_file_ids t ON t.id = c.file_id
              )
           OR parent_name_id IN (
                SELECT id
                FROM temp_ucore_changed_name_ids
              )
        "#,
        [],
    )?;

    tx.execute_batch(
        "DROP TABLE IF EXISTS temp_ucore_changed_file_ids;
         DROP TABLE IF EXISTS temp_ucore_changed_name_ids;",
    )?;
    Ok(())
}

fn resolve_file_includes_incremental_tx(
    tx: &rusqlite::Transaction,
    changed_file_ids: &[i64],
    affected_filename_ids: &[i64],
) -> anyhow::Result<()> {
    if changed_file_ids.is_empty() && affected_filename_ids.is_empty() {
        return Ok(());
    }

    populate_temp_ids_table(tx, "temp_ucore_include_file_ids", changed_file_ids)?;
    populate_temp_ids_table(tx, "temp_ucore_include_name_ids", affected_filename_ids)?;

    tx.execute(
        r#"
        UPDATE file_includes
        SET resolved_file_id = NULL
        WHERE file_id IN (SELECT id FROM temp_ucore_include_file_ids)
           OR resolved_file_id IN (SELECT id FROM temp_ucore_include_file_ids)
           OR base_filename_id IN (SELECT id FROM temp_ucore_include_name_ids)
        "#,
        [],
    )?;

    tx.execute(
        r#"
        UPDATE file_includes
        SET resolved_file_id = (
            SELECT f.id
            FROM files f
            WHERE f.filename_id = file_includes.base_filename_id
            LIMIT 1
        )
        WHERE (
                file_id IN (SELECT id FROM temp_ucore_include_file_ids)
                OR base_filename_id IN (SELECT id FROM temp_ucore_include_name_ids)
              )
          AND (
                SELECT COUNT(*)
                FROM files f
                WHERE f.filename_id = file_includes.base_filename_id
              ) = 1
        "#,
        [],
    )?;

    tx.execute_batch(
        "DROP TABLE IF EXISTS temp_ucore_include_file_ids;
         DROP TABLE IF EXISTS temp_ucore_include_name_ids;",
    )?;
    Ok(())
}

/// Save classes and their members.
/// 保存类及其成员。
#[allow(clippy::too_many_arguments)]
fn save_classes(
    tx: &rusqlite::Transaction,
    string_cache: &mut HashMap<String, i64>,
    stmt_class: &mut rusqlite::Statement,
    stmt_inheritance: &mut rusqlite::Statement,
    stmt_enum: &mut rusqlite::Statement,
    stmt_member: &mut rusqlite::Statement,
    stmt_fts: &mut rusqlite::Statement,
    stmt_search_symbol: &mut rusqlite::Statement,
    file_id: i64,
    module_id: Option<i64>,
    module_name: Option<&str>,
    path: &Path,
    extension: &str,
    classes: &[crate::types::ClassInfo],
) -> anyhow::Result<()> {
    for class_info in classes {
        let class_name_id = get_or_create_string(tx, string_cache, &class_info.class_name)?;
        let namespace_id = match &class_info.namespace {
            Some(namespace) => Some(get_or_create_string(tx, string_cache, namespace)?),
            None => None,
        };
        let base_class_id = match class_info.base_classes.first() {
            Some(base_class) => Some(get_or_create_string(tx, string_cache, base_class)?),
            None => None,
        };

        stmt_class.execute(params![
            class_name_id,
            namespace_id,
            base_class_id,
            file_id,
            class_info.line as i64,
            class_info.symbol_type,
            class_info.end_line as i64,
        ])?;

        let class_row_id = tx.last_insert_rowid();

        stmt_fts.execute(params![
            class_info.class_name,
            class_info.symbol_type,
            class_info.class_name,
            class_row_id,
        ])?;

        insert_search_symbol_row(
            stmt_search_symbol,
            file_id,
            class_row_id,
            "classes",
            &class_info.symbol_type,
            &class_info.class_name,
            Some(&class_info.class_name),
            module_id,
            module_name,
            path,
            extension,
            class_info.line as i64,
        )?;

        for parent in &class_info.base_classes {
            let parent_name_id = get_or_create_string(tx, string_cache, parent)?;
            stmt_inheritance.execute(params![class_row_id, parent_name_id])?;
        }

        save_members(
            tx,
            string_cache,
            stmt_enum,
            stmt_member,
            stmt_fts,
            stmt_search_symbol,
            file_id,
            class_row_id,
            class_info,
            module_id,
            module_name,
            path,
            extension,
        )?;
    }

    Ok(())
}

/// Save members for one class.
/// 保存单个类的成员。
#[allow(clippy::too_many_arguments)]
fn save_members(
    tx: &rusqlite::Transaction,
    string_cache: &mut HashMap<String, i64>,
    stmt_enum: &mut rusqlite::Statement,
    stmt_member: &mut rusqlite::Statement,
    stmt_fts: &mut rusqlite::Statement,
    stmt_search_symbol: &mut rusqlite::Statement,
    file_id: i64,
    class_row_id: i64,
    class_info: &crate::types::ClassInfo,
    module_id: Option<i64>,
    module_name: Option<&str>,
    path: &Path,
    extension: &str,
) -> anyhow::Result<()> {
    for member in &class_info.members {
        let member_name_id = get_or_create_string(tx, string_cache, &member.name)?;

        if member.mem_type == "enum_item" {
            stmt_enum.execute(params![
                class_row_id,
                member_name_id,
                member.line as i64,
                file_id,
            ])?;
            continue;
        }

        let type_id = get_or_create_string(tx, string_cache, &member.mem_type)?;
        let return_type_id = match &member.return_type {
            Some(return_type) => Some(get_or_create_string(tx, string_cache, return_type)?),
            None => None,
        };

        stmt_member.execute(params![
            class_row_id,
            member_name_id,
            type_id,
            member.flags,
            member.access,
            member.detail,
            return_type_id,
            member.flags.contains("static") as i32,
            member.line as i64,
            file_id,
        ])?;

        let member_row_id = tx.last_insert_rowid();

        stmt_fts.execute(params![
            member.name,
            member.mem_type,
            class_info.class_name,
            member_row_id,
        ])?;

        insert_search_symbol_row(
            stmt_search_symbol,
            file_id,
            member_row_id,
            "members",
            &member.mem_type,
            &member.name,
            Some(&class_info.class_name),
            module_id,
            module_name,
            path,
            extension,
            member.line as i64,
        )?;
    }

    Ok(())
}

/// Save function/member calls.
/// 保存函数和成员调用。
fn save_calls(
    tx: &rusqlite::Transaction,
    string_cache: &mut HashMap<String, i64>,
    stmt_call: &mut rusqlite::Statement,
    stmt_search_call: &mut rusqlite::Statement,
    file_id: i64,
    module_id: Option<i64>,
    module_name: Option<&str>,
    path: &Path,
    calls: &[crate::types::CallInfo],
) -> anyhow::Result<()> {
    for call in calls {
        let name_id = get_or_create_string(tx, string_cache, &call.name)?;
        stmt_call.execute(params![file_id, call.line as i64, name_id])?;
        insert_search_symbol_call_row(
            stmt_search_call,
            file_id,
            module_id,
            module_name,
            path,
            call.line as i64,
            &call.name,
        )?;
    }

    Ok(())
}

/// Save include relationships.
/// 保存 include 关系。
fn save_includes(
    tx: &rusqlite::Transaction,
    string_cache: &mut HashMap<String, i64>,
    stmt_include: &mut rusqlite::Statement,
    file_id: i64,
    includes: &[String],
) -> anyhow::Result<()> {
    for include_path in includes {
        let include_path_id = get_or_create_string(tx, string_cache, include_path)?;
        let base_filename = Path::new(include_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(include_path);
        let base_filename_id = get_or_create_string(tx, string_cache, base_filename)?;

        stmt_include.execute(params![
            file_id,
            include_path_id,
            base_filename_id,
        ])?;
    }

    Ok(())
}

fn save_gameplay_tags(
    stmt_tag: &mut rusqlite::Statement,
    file_id: i64,
    tags: &[crate::types::GameplayTagInfo],
) -> anyhow::Result<()> {
    for tag in tags {
        stmt_tag.execute(params![
            tag.identifier,
            tag.tag_path,
            tag.kind,
            tag.line as i64,
            file_id,
        ])?;
    }

    Ok(())
}

fn save_macro_definitions(
    stmt_macro: &mut rusqlite::Statement,
    file_id: i64,
    macros: &[crate::types::MacroDefinitionInfo],
) -> anyhow::Result<()> {
    for macro_info in macros {
        stmt_macro.execute(params![
            macro_info.name,
            macro_info.line as i64,
            file_id,
        ])?;
    }

    Ok(())
}

/// Resolve inheritance rows to actual class ids when possible.
/// 尽量把继承关系解析到真实 class id。
fn resolve_inheritance(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        UPDATE inheritance
        SET parent_class_id = (
            SELECT c.id
            FROM classes c
            WHERE c.name_id = inheritance.parent_name_id
            LIMIT 1
        )
        WHERE parent_class_id IS NULL
        "#,
        [],
    )?;

    Ok(())
}

/// Resolve includes when the included base filename is unique.
/// 当 include 的文件名唯一时，解析到具体文件。
fn resolve_file_includes_by_path(conn: &mut Connection) -> anyhow::Result<()> {
    conn.execute(
        r#"
        UPDATE file_includes
        SET resolved_file_id = (
            SELECT f.id
            FROM files f
            WHERE f.filename_id = file_includes.base_filename_id
            LIMIT 1
        )
        WHERE resolved_file_id IS NULL
          AND (
            SELECT COUNT(*)
            FROM files f
            WHERE f.filename_id = file_includes.base_filename_id
          ) = 1
        "#,
        [],
    )?;

    Ok(())
}

/// Register or update one Unreal module.
/// 注册或更新 Unreal 模块。
pub fn register_module(
    conn: &Connection,
    name: &str,
    root_path: &str,
    module_type: &str,
    scope: &str,
) -> anyhow::Result<i64> {
    let tx = conn.unchecked_transaction()?;
    let mut string_cache = HashMap::new();
    let mut dir_cache = HashMap::new();

    let name_id = get_or_create_string(&tx, &mut string_cache, name)?;
    let root_dir_id = project_path::get_or_create_directory(
        &tx,
        &mut string_cache,
        &mut dir_cache,
        Path::new(root_path),
    )?;

    tx.execute(
        "INSERT OR REPLACE INTO modules (name_id, root_directory_id, type, scope)
         VALUES (?1, ?2, ?3, ?4)",
        params![name_id, root_dir_id, module_type, scope],
    )?;

    let module_id = tx.last_insert_rowid();
    tx.commit()?;

    Ok(module_id)
}

/// Find the best module for one file path by longest root prefix.
/// 通过最长 root 前缀匹配文件所属模块。
pub fn get_module_id_for_path(
    conn: &Connection,
    file_path: &str,
) -> anyhow::Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id, root_directory_id FROM modules")?;
    let mut rows = stmt.query([])?;

    let file_path_norm = normalize_path_for_compare(file_path);
    let mut best_id = None;
    let mut best_len = 0;

    while let Some(row) = rows.next()? {
        let module_id: i64 = row.get(0)?;
        let root_directory_id: i64 = row.get(1)?;

        let root_path = project_path::get_directory_path(conn, root_directory_id)?;
        let root_path_norm = normalize_path_for_compare(&root_path);

        if file_path_norm.starts_with(&root_path_norm) && root_path_norm.len() > best_len {
            best_id = Some(module_id);
            best_len = root_path_norm.len();
        }
    }

    Ok(best_id)
}

/// Return registered Unreal components as JSON.
/// 以 JSON 返回已注册的 Unreal components。
pub fn get_components(conn: &Connection) -> anyhow::Result<serde_json::Value> {
    let mut stmt = conn.prepare(
        "SELECT
            name,
            display_name,
            type,
            owner_name,
            root_path,
            uplugin_path,
            uproject_path,
            engine_association
         FROM components",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "name": row.get::<_, String>(0)?,
            "display_name": row.get::<_, Option<String>>(1)?,
            "type": row.get::<_, Option<String>>(2)?,
            "owner_name": row.get::<_, Option<String>>(3)?,
            "root_path": row.get::<_, Option<String>>(4)?,
            "uplugin_path": row.get::<_, Option<String>>(5)?,
            "uproject_path": row.get::<_, Option<String>>(6)?,
            "engine_association": row.get::<_, Option<String>>(7)?,
        }))
    })?;

    let components: Vec<_> = rows.filter_map(Result::ok).collect();
    Ok(serde_json::json!(components))
}

/// Initialize the persistent completion cache.
/// 初始化持久化补全缓存。
pub fn init_cache_db(conn: &Connection) -> rusqlite::Result<()> {
    create_tables(conn)?;
    create_indices(conn)?;

    let stored_version = conn
        .query_row(
            "SELECT value FROM cache_meta WHERE key = 'completion_cache_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i32>().ok());

    if stored_version != Some(COMPLETION_CACHE_VERSION) {
        conn.execute("DELETE FROM persistent_cache", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO cache_meta (key, value)
             VALUES ('completion_cache_version', ?1)",
            [COMPLETION_CACHE_VERSION.to_string()],
        )?;

        info!(
            "Completion cache version changed ({:?} -> {}), cache cleared.",
            stored_version, COMPLETION_CACHE_VERSION
        );
    }

    Ok(())
}

fn is_header_extension(extension: &str) -> bool {
    matches!(extension, "h" | "hpp" | "hh" | "inl")
}

fn normalize_path_for_compare(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}
