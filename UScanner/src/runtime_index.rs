use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::query::goto::NavigationHotIndex;
use crate::query::search::SearchHotIndex;
use crate::query::usage::UsageHotIndex;

const RUNTIME_INDEX_SCHEMA_VERSION: u32 = 1;
pub const NAVIGATION_INDEX_VERSION: u32 = 2;
pub const SEARCH_INDEX_VERSION: u32 = 2;
pub const USAGE_INDEX_VERSION: u32 = 2;
pub const ASSET_INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssetRuntimeIndex {
    pub references: std::collections::HashMap<String, Vec<String>>,
    pub derived: std::collections::HashMap<String, Vec<String>>,
    pub functions: std::collections::HashMap<String, Vec<String>>,
}

impl AssetRuntimeIndex {
    pub fn size_hint(&self) -> usize {
        self.references.len() + self.derived.len() + self.functions.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RuntimeIndexManifest {
    schema_version: u32,
    db_size: u64,
    db_mtime_secs: u64,
    navigation_version: Option<u32>,
    search_version: Option<u32>,
    usage_version: Option<u32>,
    asset_version: Option<u32>,
}

#[derive(Clone, Copy)]
enum RuntimeIndexKind {
    Navigation,
    Search,
    Usage,
    Asset,
}

pub fn load_navigation_index(primary_db_path: &str) -> Result<Option<NavigationHotIndex>> {
    load_index(
        primary_db_path,
        RuntimeIndexKind::Navigation,
        NAVIGATION_INDEX_VERSION,
    )
}

pub fn save_navigation_index(primary_db_path: &str, index: &NavigationHotIndex) -> Result<()> {
    save_index(
        primary_db_path,
        RuntimeIndexKind::Navigation,
        NAVIGATION_INDEX_VERSION,
        index,
    )
}

pub fn load_search_index(primary_db_path: &str) -> Result<Option<SearchHotIndex>> {
    load_index(
        primary_db_path,
        RuntimeIndexKind::Search,
        SEARCH_INDEX_VERSION,
    )
}

pub fn save_search_index(primary_db_path: &str, index: &SearchHotIndex) -> Result<()> {
    save_index(
        primary_db_path,
        RuntimeIndexKind::Search,
        SEARCH_INDEX_VERSION,
        index,
    )
}

pub fn load_usage_index(primary_db_path: &str) -> Result<Option<UsageHotIndex>> {
    load_index(
        primary_db_path,
        RuntimeIndexKind::Usage,
        USAGE_INDEX_VERSION,
    )
}

pub fn save_usage_index(primary_db_path: &str, index: &UsageHotIndex) -> Result<()> {
    save_index(
        primary_db_path,
        RuntimeIndexKind::Usage,
        USAGE_INDEX_VERSION,
        index,
    )
}

pub fn load_asset_index(primary_db_path: &str) -> Result<Option<AssetRuntimeIndex>> {
    load_index(
        primary_db_path,
        RuntimeIndexKind::Asset,
        ASSET_INDEX_VERSION,
    )
}

pub fn save_asset_index(primary_db_path: &str, index: &AssetRuntimeIndex) -> Result<()> {
    save_index(
        primary_db_path,
        RuntimeIndexKind::Asset,
        ASSET_INDEX_VERSION,
        index,
    )
}

fn load_index<T: DeserializeOwned>(
    primary_db_path: &str,
    kind: RuntimeIndexKind,
    expected_version: u32,
) -> Result<Option<T>> {
    let manifest = match load_manifest(primary_db_path)? {
        Some(manifest) => manifest,
        None => return Ok(None),
    };

    if !manifest_matches_db(primary_db_path, &manifest)? {
        return Ok(None);
    }

    let version_matches = match kind {
        RuntimeIndexKind::Navigation => manifest.navigation_version == Some(expected_version),
        RuntimeIndexKind::Search => manifest.search_version == Some(expected_version),
        RuntimeIndexKind::Usage => manifest.usage_version == Some(expected_version),
        RuntimeIndexKind::Asset => manifest.asset_version == Some(expected_version),
    };
    if !version_matches {
        return Ok(None);
    }

    let index_path = index_file_path(primary_db_path, kind);
    if !index_path.is_file() {
        return Ok(None);
    }

    let file = File::open(&index_path)
        .with_context(|| format!("failed to open runtime index {}", index_path.display()))?;
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("failed to mmap runtime index {}", index_path.display()))?;
    let value = rmp_serde::from_slice(&mmap)
        .with_context(|| format!("failed to decode runtime index {}", index_path.display()))?;
    Ok(Some(value))
}

fn save_index<T: Serialize>(
    primary_db_path: &str,
    kind: RuntimeIndexKind,
    version: u32,
    value: &T,
) -> Result<()> {
    let index_dir = ensure_index_dir(primary_db_path)?;
    let manifest_path = manifest_path(primary_db_path);
    let index_path = index_file_path(primary_db_path, kind);
    let temp_path = index_dir.join(format!(
        "{}.tmp",
        index_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("index.bin")
    ));

    let bytes = rmp_serde::to_vec(value)?;
    fs::write(&temp_path, bytes)
        .with_context(|| format!("failed to write runtime index {}", temp_path.display()))?;
    fs::rename(&temp_path, &index_path).with_context(|| {
        format!(
            "failed to rename runtime index {} -> {}",
            temp_path.display(),
            index_path.display()
        )
    })?;

    let metadata = fs::metadata(primary_db_path)
        .with_context(|| format!("failed to stat primary db {}", primary_db_path))?;
    let mut manifest = load_manifest(primary_db_path)?.unwrap_or_default();
    manifest.schema_version = RUNTIME_INDEX_SCHEMA_VERSION;
    manifest.db_size = metadata.len();
    manifest.db_mtime_secs = modified_secs(&metadata)?;
    match kind {
        RuntimeIndexKind::Navigation => manifest.navigation_version = Some(version),
        RuntimeIndexKind::Search => manifest.search_version = Some(version),
        RuntimeIndexKind::Usage => manifest.usage_version = Some(version),
        RuntimeIndexKind::Asset => manifest.asset_version = Some(version),
    }

    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    fs::write(&manifest_path, manifest_json)
        .with_context(|| format!("failed to write manifest {}", manifest_path.display()))?;
    Ok(())
}

fn load_manifest(primary_db_path: &str) -> Result<Option<RuntimeIndexManifest>> {
    let path = manifest_path(primary_db_path);
    if !path.is_file() {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read runtime index manifest {}", path.display()))?;
    let manifest: RuntimeIndexManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse runtime index manifest {}", path.display()))?;
    if manifest.schema_version != RUNTIME_INDEX_SCHEMA_VERSION {
        return Ok(None);
    }
    Ok(Some(manifest))
}

fn manifest_matches_db(primary_db_path: &str, manifest: &RuntimeIndexManifest) -> Result<bool> {
    let metadata = fs::metadata(primary_db_path)
        .with_context(|| format!("failed to stat primary db {}", primary_db_path))?;
    Ok(metadata.len() == manifest.db_size && modified_secs(&metadata)? == manifest.db_mtime_secs)
}

fn modified_secs(metadata: &fs::Metadata) -> Result<u64> {
    Ok(metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn ensure_index_dir(primary_db_path: &str) -> Result<PathBuf> {
    let path = index_dir(primary_db_path);
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create runtime index dir {}", path.display()))?;
    Ok(path)
}

fn manifest_path(primary_db_path: &str) -> PathBuf {
    index_dir(primary_db_path).join("manifest.json")
}

fn index_file_path(primary_db_path: &str, kind: RuntimeIndexKind) -> PathBuf {
    let file_name = match kind {
        RuntimeIndexKind::Navigation => "nav.idx",
        RuntimeIndexKind::Search => "symbol.idx",
        RuntimeIndexKind::Usage => "usage.idx",
        RuntimeIndexKind::Asset => "asset.idx",
    };
    index_dir(primary_db_path).join(file_name)
}

fn index_dir(primary_db_path: &str) -> PathBuf {
    let db_path = Path::new(primary_db_path);
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = db_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("ucore");
    let mut hasher = Sha256::new();
    hasher.update(primary_db_path.as_bytes());
    let digest = hex::encode(hasher.finalize());
    parent
        .join(".ucore")
        .join("index")
        .join(format!("{}-{}", stem, &digest[..12]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::query::goto::build_navigation_hot_index;
    use crate::query::search::build_search_hot_index;
    use crate::query::usage::build_usage_hot_index;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_base(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ucore-runtime-index-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn save_and_load_navigation_index_round_trips() {
        let base = temp_base("nav");
        fs::create_dir_all(&base).unwrap();
        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();

        let nav_index = build_navigation_hot_index(&conn).unwrap();
        save_navigation_index(db_path.to_string_lossy().as_ref(), &nav_index).unwrap();
        let loaded = load_navigation_index(db_path.to_string_lossy().as_ref())
            .unwrap()
            .expect("navigation index should load");

        assert_eq!(loaded.size_hint(), nav_index.size_hint());

        let _ = fs::remove_dir_all(base.join(".ucore"));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn save_and_load_search_index_round_trips() {
        let base = temp_base("search");
        fs::create_dir_all(&base).unwrap();
        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();

        let search_index = build_search_hot_index(&conn).unwrap();
        save_search_index(db_path.to_string_lossy().as_ref(), &search_index).unwrap();
        let loaded = load_search_index(db_path.to_string_lossy().as_ref())
            .unwrap()
            .expect("search index should load");

        assert_eq!(loaded.size_hint(), search_index.size_hint());

        let _ = fs::remove_dir_all(base.join(".ucore"));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn save_and_load_usage_index_round_trips() {
        let base = temp_base("usage");
        fs::create_dir_all(&base).unwrap();
        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();

        let usage_index = build_usage_hot_index(&conn).unwrap();
        save_usage_index(db_path.to_string_lossy().as_ref(), &usage_index).unwrap();
        let loaded = load_usage_index(db_path.to_string_lossy().as_ref())
            .unwrap()
            .expect("usage index should load");

        assert_eq!(loaded.size_hint(), usage_index.size_hint());

        let _ = fs::remove_dir_all(base.join(".ucore"));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_dir(&base);
    }

    #[test]
    fn save_and_load_asset_index_round_trips() {
        let base = temp_base("asset");
        fs::create_dir_all(&base).unwrap();
        let db_path = base.join("ucore.db");
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();

        let mut index = AssetRuntimeIndex::default();
        index.references.insert(
            "weaponforgemain".to_string(),
            vec!["/game/ui/wbp_weaponforge".to_string()],
        );
        index.derived.insert(
            "uweaponforgemain".to_string(),
            vec!["/game/ui/wbp_weaponforge".to_string()],
        );
        index.functions.insert(
            "nativeconstruct".to_string(),
            vec!["/game/ui/wbp_weaponforge".to_string()],
        );

        save_asset_index(db_path.to_string_lossy().as_ref(), &index).unwrap();
        let loaded = load_asset_index(db_path.to_string_lossy().as_ref())
            .unwrap()
            .expect("asset index should load");

        assert_eq!(loaded.size_hint(), index.size_hint());

        let _ = fs::remove_dir_all(base.join(".ucore"));
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_dir(&base);
    }
}
