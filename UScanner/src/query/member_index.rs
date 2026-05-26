use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberHotIndex {
    class_name_by_id: HashMap<i64, String>,
    members_by_class: HashMap<i64, Vec<MemberEntry>>,
    enum_values_by_class: HashMap<i64, Vec<EnumValueEntry>>,
    parents_by_class: HashMap<i64, Vec<ParentEntry>>,
    class_ids_by_name: HashMap<String, Vec<i64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassEntry {
    pub id: i64,
    pub name: String,
    pub file_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberEntry {
    pub owner_class_id: i64,
    pub owner_class_name: String,
    pub name: String,
    pub member_type: String,
    pub return_type: Option<String>,
    pub access: Option<String>,
    pub detail: Option<String>,
    pub is_static: bool,
    pub line: Option<usize>,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValueEntry {
    pub owner_class_id: i64,
    pub owner_class_name: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParentEntry {
    pub parent_class_id: Option<i64>,
    pub parent_name: String,
}

#[derive(Debug, Clone)]
pub enum HotMemberItem<'a> {
    Member {
        entry: &'a MemberEntry,
        class_rank: usize,
    },
    EnumValue {
        entry: &'a EnumValueEntry,
        class_rank: usize,
    },
}

impl MemberHotIndex {
    pub fn size_hint(&self) -> usize {
        self.class_name_by_id.len()
            + self.members_by_class.values().map(Vec::len).sum::<usize>()
            + self.enum_values_by_class.values().map(Vec::len).sum::<usize>()
            + self.parents_by_class.values().map(Vec::len).sum::<usize>()
            + self.class_ids_by_name.len()
    }

    pub fn class_ids_by_name(&self, class_name: &str) -> Vec<i64> {
        let class_name = clean_type(class_name);
        if class_name.is_empty() {
            return Vec::new();
        }

        if let Some(ids) = self.class_ids_by_name.get(&class_name) {
            return ids.clone();
        }

        if class_name.contains("::") {
            let short = class_name.rsplit("::").next().unwrap_or(&class_name);
            if let Some(ids) = self.class_ids_by_name.get(short) {
                return ids.clone();
            }
        }

        Vec::new()
    }

    pub fn class_name_by_id(&self, class_id: i64) -> Option<&str> {
        self.class_name_by_id.get(&class_id).map(|text| text.as_str())
    }

    pub fn parent_classes(&self, class_id: i64) -> &[ParentEntry] {
        self.parents_by_class
            .get(&class_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn collect_members_recursive<'a>(
        &'a self,
        class_name: &str,
        include_impl_members: bool,
        limit: usize,
    ) -> Vec<HotMemberItem<'a>> {
        let mut class_ids = self.class_ids_by_name(class_name);
        if class_ids.is_empty() {
            if let Some(base) = clean_type(class_name).split('<').next() {
                class_ids = self.class_ids_by_name(base);
            }
        }

        self.collect_members_recursive_from_ids(&class_ids, include_impl_members, limit)
    }

    pub fn collect_members_recursive_from_ids<'a>(
        &'a self,
        class_ids: &[i64],
        include_impl_members: bool,
        limit: usize,
    ) -> Vec<HotMemberItem<'a>> {
        let mut queue = VecDeque::from(
            class_ids
                .iter()
                .copied()
                .map(|class_id| (class_id, 0usize))
                .collect::<Vec<_>>(),
        );
        let mut visited_ids = HashSet::new();
        let mut visited_names = HashSet::new();
        let mut items = Vec::new();

        while let Some((class_id, class_rank)) = queue.pop_front() {
            if !visited_ids.insert(class_id) {
                continue;
            }

            if let Some(name) = self.class_name_by_id(class_id) {
                visited_names.insert(name.to_string());
            }

            if let Some(members) = self.members_by_class.get(&class_id) {
                for entry in members {
                    if !include_impl_members && entry.access.as_deref() == Some("impl") {
                        continue;
                    }
                    items.push(HotMemberItem::Member { entry, class_rank });
                    if items.len() >= limit {
                        return items;
                    }
                }
            }

            if let Some(values) = self.enum_values_by_class.get(&class_id) {
                for entry in values {
                    items.push(HotMemberItem::EnumValue { entry, class_rank });
                    if items.len() >= limit {
                        return items;
                    }
                }
            }

            for parent in self.parent_classes(class_id) {
                if !parent.parent_name.is_empty() && !visited_names.insert(parent.parent_name.clone()) {
                    continue;
                }

                if let Some(parent_id) = parent.parent_class_id {
                    queue.push_back((parent_id, class_rank + 1));
                }

                for id in self.class_ids_by_name(&parent.parent_name) {
                    queue.push_back((id, class_rank + 1));
                }
            }
        }

        items
    }

    pub fn is_subclass_of(&self, child: &str, parent: &str) -> bool {
        if clean_type(child) == clean_type(parent) {
            return true;
        }

        let parent_ids = self.class_ids_by_name(parent);
        if parent_ids.is_empty() {
            return false;
        }

        let mut queue = VecDeque::from(self.class_ids_by_name(child));
        let mut visited = HashSet::new();

        while let Some(class_id) = queue.pop_front() {
            if parent_ids.contains(&class_id) {
                return true;
            }

            if !visited.insert(class_id) {
                continue;
            }

            let target_parent = clean_type(parent);
            for parent_entry in self.parent_classes(class_id) {
                if clean_type(&parent_entry.parent_name) == target_parent {
                    return true;
                }

                if let Some(parent_id) = parent_entry.parent_class_id {
                    queue.push_back(parent_id);
                }

                for id in self.class_ids_by_name(&parent_entry.parent_name) {
                    queue.push_back(id);
                }
            }
        }

        false
    }

    pub fn is_member_accessible(
        &self,
        owner_class: &str,
        accessor_class: &str,
        access: Option<&str>,
        assume_subclass_access: bool,
    ) -> bool {
        let access = access.unwrap_or("");

        if accessor_class.is_empty() {
            return access.is_empty() || access == "public";
        }

        if accessor_class == owner_class {
            return true;
        }

        if access == "private" {
            return false;
        }

        if access == "protected" {
            if assume_subclass_access {
                return true;
            }
            return self.is_subclass_of(accessor_class, owner_class);
        }

        true
    }
}

pub fn build_member_hot_index(conn: &Connection) -> Result<MemberHotIndex> {
    let mut class_name_by_id = HashMap::<i64, String>::new();
    let mut class_ids_by_name = HashMap::<String, Vec<i64>>::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT c.id, sn.text, c.file_id
        FROM classes c
        JOIN strings sn ON c.name_id = sn.id
        ORDER BY sn.text, c.line_number
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ClassEntry {
            id: row.get(0)?,
            name: row.get(1)?,
            file_id: row.get(2)?,
        })
    })?;
    for row in rows {
        let entry = row?;
        class_ids_by_name
            .entry(entry.name.clone())
            .or_default()
            .push(entry.id);
        class_name_by_id.insert(entry.id, entry.name);
    }

    let mut members_by_class = HashMap::<i64, Vec<MemberEntry>>::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT
            m.class_id,
            sc.text,
            smn.text,
            smt.text,
            srt.text,
            m.access,
            m.detail,
            m.is_static,
            m.line_number,
            CASE
                WHEN dp.full_path IS NULL OR sn.text IS NULL THEN NULL
                WHEN dp.full_path = '/' THEN '/' || sn.text
                WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sn.text
                ELSE dp.full_path || '/' || sn.text
            END
        FROM members m
        JOIN classes c ON m.class_id = c.id
        JOIN strings sc ON c.name_id = sc.id
        JOIN strings smn ON m.name_id = smn.id
        JOIN strings smt ON m.type_id = smt.id
        LEFT JOIN strings srt ON m.return_type_id = srt.id
        LEFT JOIN files f ON m.file_id = f.id
        LEFT JOIN dir_paths dp ON f.directory_id = dp.id
        LEFT JOIN strings sn ON f.filename_id = sn.id
        ORDER BY m.class_id, smn.text
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MemberEntry {
            owner_class_id: row.get(0)?,
            owner_class_name: row.get(1)?,
            name: row.get(2)?,
            member_type: row.get(3)?,
            return_type: row.get(4)?,
            access: row.get(5)?,
            detail: row.get(6)?,
            is_static: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
            line: row.get::<_, Option<i64>>(8)?.map(|value| value as usize),
            file_path: row.get::<_, Option<String>>(9)?.map(|path| path.replace('\\', "/")),
        })
    })?;
    for row in rows {
        let entry = row?;
        members_by_class
            .entry(entry.owner_class_id)
            .or_default()
            .push(entry);
    }

    let mut enum_values_by_class = HashMap::<i64, Vec<EnumValueEntry>>::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT ev.enum_id, sc.text, sen.text
        FROM enum_values ev
        JOIN classes c ON ev.enum_id = c.id
        JOIN strings sc ON c.name_id = sc.id
        JOIN strings sen ON ev.name_id = sen.id
        ORDER BY ev.enum_id, sen.text
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EnumValueEntry {
            owner_class_id: row.get(0)?,
            owner_class_name: row.get(1)?,
            name: row.get(2)?,
        })
    })?;
    for row in rows {
        let entry = row?;
        enum_values_by_class
            .entry(entry.owner_class_id)
            .or_default()
            .push(entry);
    }

    let mut parents_by_class = HashMap::<i64, Vec<ParentEntry>>::new();
    let mut stmt = conn.prepare(
        r#"
        SELECT i.child_id, i.parent_class_id, sp.text
        FROM inheritance i
        JOIN strings sp ON i.parent_name_id = sp.id
        ORDER BY i.child_id
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            ParentEntry {
                parent_class_id: row.get(1)?,
                parent_name: row.get(2)?,
            },
        ))
    })?;
    for row in rows {
        let (child_id, entry) = row?;
        parents_by_class.entry(child_id).or_default().push(entry);
    }

    Ok(MemberHotIndex {
        class_name_by_id,
        members_by_class,
        enum_values_by_class,
        parents_by_class,
        class_ids_by_name,
    })
}

fn clean_type(input: &str) -> String {
    let mut text = input.trim().trim_start_matches("class ").trim_start_matches("struct ").trim();

    while let Some(stripped) = text.strip_prefix("const ") {
        text = stripped.trim();
    }

    text.trim_matches('&')
        .trim_matches('*')
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::params;
    use rusqlite::Connection;

    #[test]
    fn member_hot_index_collects_inherited_members() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();
        conn.execute("INSERT INTO strings (text) VALUES ('UBase'), ('UChild'), ('ParentFn'), ('ChildFn'), ('function')", [])
            .unwrap();
        let base_name: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'UBase'", [], |row| row.get(0))
            .unwrap();
        let child_name: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'UChild'", [], |row| row.get(0))
            .unwrap();
        let parent_fn: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'ParentFn'", [], |row| row.get(0))
            .unwrap();
        let child_fn: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'ChildFn'", [], |row| row.get(0))
            .unwrap();
        let function_type: i64 = conn
            .query_row("SELECT id FROM strings WHERE text = 'function'", [], |row| row.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO classes (name_id, line_number, symbol_type) VALUES (?1, 1, 'class')",
            [base_name],
        )
        .unwrap();
        let base_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO classes (name_id, line_number, symbol_type) VALUES (?1, 1, 'class')",
            [child_name],
        )
        .unwrap();
        let child_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO inheritance (child_id, parent_name_id, parent_class_id) VALUES (?1, ?2, ?3)",
            params![child_id, base_name, base_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (class_id, name_id, type_id, access) VALUES (?1, ?2, ?3, 'public')",
            params![base_id, parent_fn, function_type],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (class_id, name_id, type_id, access) VALUES (?1, ?2, ?3, 'public')",
            params![child_id, child_fn, function_type],
        )
        .unwrap();

        let index = build_member_hot_index(&conn).unwrap();
        let items = index.collect_members_recursive("UChild", true, 128);
        let names = items
            .iter()
            .filter_map(|item| match item {
                HotMemberItem::Member { entry, .. } => Some(entry.name.as_str()),
                HotMemberItem::EnumValue { .. } => None,
            })
            .collect::<Vec<_>>();

        assert!(names.contains(&"ChildFn"));
        assert!(names.contains(&"ParentFn"));
    }
}
