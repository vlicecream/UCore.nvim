use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::db::text;

pub const ASSET_DB_VERSION: i32 = 1;

const ASSET_DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

pub fn derived_asset_db_path(primary_db_path: &str) -> String {
    let path = PathBuf::from(primary_db_path);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("ucore");
    parent
        .join(format!("{}-asset.db", stem))
        .to_string_lossy()
        .to_string()
}

pub fn ensure_asset_db(primary_db_path: &str) -> Result<String> {
    let asset_db_path = derived_asset_db_path(primary_db_path);
    ensure_asset_db_version(&asset_db_path)?;
    Ok(asset_db_path)
}

pub fn init_asset_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(ASSET_DB_BUSY_TIMEOUT)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_meta (
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

        CREATE TABLE IF NOT EXISTS search_asset_usages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            lookup_key TEXT NOT NULL,
            usage_kind TEXT NOT NULL,
            asset_path TEXT NOT NULL,
            asset_name TEXT NOT NULL,
            asset_name_lc TEXT NOT NULL,
            source_path TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_assets_asset_key ON assets(asset_key);
        CREATE INDEX IF NOT EXISTS idx_assets_source_path_key ON assets(source_path_key);
        CREATE INDEX IF NOT EXISTS idx_assets_parent_class_key ON assets(parent_class_key);
        CREATE INDEX IF NOT EXISTS idx_asset_references_asset_path ON asset_references(asset_path);
        CREATE INDEX IF NOT EXISTS idx_asset_references_reference_key ON asset_references(reference_key);
        CREATE INDEX IF NOT EXISTS idx_asset_functions_asset_path ON asset_functions(asset_path);
        CREATE INDEX IF NOT EXISTS idx_asset_functions_function_key ON asset_functions(function_key);
        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_lookup_kind ON search_asset_usages(lookup_key, usage_kind);
        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_path ON search_asset_usages(asset_path);
        CREATE INDEX IF NOT EXISTS idx_search_asset_usages_asset_name_lc ON search_asset_usages(asset_name_lc);
        "#,
    )?;

    conn.execute(
        "INSERT OR REPLACE INTO asset_meta (key, value) VALUES ('db_version', ?1)",
        [ASSET_DB_VERSION.to_string()],
    )?;

    Ok(())
}

pub fn current_asset_db_path(primary_conn: &Connection) -> Result<Option<String>> {
    let Some(primary_db_path) = text::current_primary_db_path(primary_conn)? else {
        return Ok(None);
    };
    Ok(Some(derived_asset_db_path(&primary_db_path)))
}

pub fn open_asset_db_for_primary(primary_conn: &Connection) -> Result<Option<Connection>> {
    let Some(primary_db_path) = text::current_primary_db_path(primary_conn)? else {
        return Ok(None);
    };
    let asset_db_path = ensure_asset_db(&primary_db_path)?;
    let conn = Connection::open(&asset_db_path)
        .with_context(|| format!("failed to open asset database {}", asset_db_path))?;
    init_asset_db(&conn)?;
    Ok(Some(conn))
}

pub fn open_asset_db_read_only_for_primary(primary_conn: &Connection) -> Result<Option<Connection>> {
    let Some(primary_db_path) = text::current_primary_db_path(primary_conn)? else {
        return Ok(None);
    };

    let asset_db_path = derived_asset_db_path(&primary_db_path);
    if !Path::new(&asset_db_path).is_file() {
        return Ok(None);
    }

    let conn = Connection::open_with_flags(
        &asset_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open asset database {}", asset_db_path))?;
    conn.busy_timeout(ASSET_DB_BUSY_TIMEOUT)?;
    Ok(Some(conn))
}

pub fn asset_db_version_matches(asset_db_path: &str) -> Result<bool> {
    let conn = Connection::open(asset_db_path)
        .with_context(|| format!("failed to open asset database {}", asset_db_path))?;
    let version = conn
        .query_row(
            "SELECT value FROM asset_meta WHERE key = 'db_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i32>().ok());
    Ok(version == Some(ASSET_DB_VERSION))
}

fn ensure_asset_db_version(asset_db_path: &str) -> Result<bool> {
    let db_exists = Path::new(asset_db_path).exists();
    if db_exists && asset_db_version_matches(asset_db_path)? {
        return Ok(false);
    }

    if db_exists {
        std::fs::remove_file(asset_db_path)
            .with_context(|| format!("failed to remove old asset database {}", asset_db_path))?;
    }

    let conn = Connection::open(asset_db_path)
        .with_context(|| format!("failed to open asset database {}", asset_db_path))?;
    init_asset_db(&conn)?;
    Ok(true)
}
