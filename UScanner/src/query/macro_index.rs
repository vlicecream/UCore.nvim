use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::query::member_index::{StrId, StringPool};

#[derive(Debug, Serialize, Deserialize)]
pub struct MacroHotIndex {
    pool: StringPool,
    entries: Vec<MacroEntry>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct MacroEntry {
    name: StrId,
    name_lc: StrId,
    parameters: StrId,
    detail: StrId,
    is_function_like: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroCandidate {
    pub name: String,
    pub parameters: Option<String>,
    pub detail: Option<String>,
    pub is_function_like: bool,
}

impl MacroHotIndex {
    pub fn size_hint(&self) -> usize {
        self.entries.len()
    }

    pub fn lookup_by_prefix(&self, prefix: &str, limit: usize) -> Vec<MacroCandidate> {
        let prefix = prefix.trim();
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }

        let prefix_lc = prefix.to_ascii_lowercase();
        let start = self
            .entries
            .partition_point(|entry| self.text(entry.name_lc) < prefix_lc.as_str());

        let mut out = Vec::with_capacity(limit);
        for entry in &self.entries[start..] {
            let name_lc = self.text(entry.name_lc);
            if !name_lc.starts_with(&prefix_lc) {
                break;
            }

            out.push(MacroCandidate {
                name: self.text(entry.name).to_string(),
                parameters: self.text_opt(entry.parameters).map(str::to_string),
                detail: self.text_opt(entry.detail).map(str::to_string),
                is_function_like: entry.is_function_like,
            });

            if out.len() >= limit {
                break;
            }
        }

        out
    }

    fn text(&self, id: StrId) -> &str {
        self.pool.get(id).unwrap_or("")
    }

    fn text_opt(&self, id: StrId) -> Option<&str> {
        self.pool.get(id).filter(|text| !text.is_empty())
    }
}

pub fn build_macro_hot_index(conn: &Connection) -> Result<MacroHotIndex> {
    let mut stmt = conn.prepare(
        r#"
        SELECT name, is_function_like, parameters, detail
        FROM macro_definitions
        ORDER BY lower(name), name
        "#,
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)? != 0,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut pool = StringPool::new();
    let mut entries = Vec::new();

    for row in rows {
        let (name, is_function_like, parameters, detail) = row?;
        entries.push(MacroEntry {
            name_lc: pool.intern(&name.to_ascii_lowercase()),
            name: pool.intern(&name),
            parameters: parameters
                .as_deref()
                .map(|value| pool.intern(value))
                .unwrap_or(StrId::NONE),
            detail: detail
                .as_deref()
                .map(|value| pool.intern(value))
                .unwrap_or(StrId::NONE),
            is_function_like,
        });
    }

    Ok(MacroHotIndex { pool, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn insert_test_file(conn: &Connection) {
        conn.execute("INSERT INTO strings (id, text) VALUES (1, 'C:'), (2, 'Macro.h')", [])
            .unwrap();
        conn.execute("INSERT INTO directories (id, parent_id, name_id) VALUES (1, NULL, 1)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO files (id, directory_id, filename_id, extension, is_header)
             VALUES (1, 1, 2, 'h', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn macro_hot_index_matches_prefix() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();
        insert_test_file(&conn);
        conn.execute(
            "INSERT INTO macro_definitions (name, is_function_like, parameters, detail, line_number, file_id)
             VALUES ('UPROPERTY', 1, '...', '#define UPROPERTY(...)', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO macro_definitions (name, is_function_like, parameters, detail, line_number, file_id)
             VALUES ('GENERATED_BODY', 1, '...', '#define GENERATED_BODY(...)', 2, 1)",
            [],
        )
        .unwrap();

        let index = build_macro_hot_index(&conn).unwrap();
        let items = index.lookup_by_prefix("UP", 8);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "UPROPERTY");
        assert!(items[0].is_function_like);
    }
}
