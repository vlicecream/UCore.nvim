use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct StrId(pub u32);

impl StrId {
    pub const NONE: Self = Self(0);

    fn is_none(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StringPool {
    strings: Vec<String>,
    #[serde(skip)]
    lookup: HashMap<String, StrId>,
}

impl Default for StringPool {
    fn default() -> Self {
        Self::new()
    }
}

impl StringPool {
    pub fn new() -> Self {
        Self {
            strings: vec![String::new()],
            lookup: HashMap::new(),
        }
    }

    pub fn intern(&mut self, text: &str) -> StrId {
        let text = text.trim();
        if text.is_empty() {
            return StrId::NONE;
        }

        if let Some(&id) = self.lookup.get(text) {
            return id;
        }

        let id = StrId(self.strings.len() as u32);
        self.strings.push(text.to_string());
        self.lookup.insert(text.to_string(), id);
        id
    }

    pub fn get(&self, id: StrId) -> Option<&str> {
        if id.is_none() {
            return None;
        }

        self.strings.get(id.0 as usize).map(String::as_str)
    }

    fn clear_lookup(&mut self) {
        self.lookup.clear();
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemberHotIndex {
    pool: StringPool,
    file_paths: Vec<StrId>,
    class_names: Vec<StrId>,
    members: Vec<MemberEntry>,
    member_ranges: Vec<MemberRange>,
    enum_values: Vec<EnumValueEntry>,
    enum_ranges: Vec<MemberRange>,
    parents: Vec<ParentEntry>,
    parent_ranges: Vec<MemberRange>,
    class_ids_by_name: HashMap<String, Vec<i32>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemberRange {
    pub class_id: i32,
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MemberKind {
    Function = 0,
    Property = 1,
    Event = 2,
    Variable = 3,
    Macro = 4,
    Other = 255,
}

impl MemberKind {
    fn from_text(text: &str) -> Self {
        match text {
            "function" => Self::Function,
            "property" => Self::Property,
            "event" => Self::Event,
            "variable" | "field" => Self::Variable,
            "macro" => Self::Macro,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberAccess {
    Public,
    Protected,
    Private,
    Impl,
}

pub const FLAG_STATIC: u8 = 1 << 0;
pub const FLAG_ACCESS_PUBLIC: u8 = 0 << 1;
pub const FLAG_ACCESS_PROTECTED: u8 = 1 << 1;
pub const FLAG_ACCESS_PRIVATE: u8 = 2 << 1;
pub const FLAG_ACCESS_IMPL: u8 = 3 << 1;
pub const FLAG_ACCESS_MASK: u8 = 3 << 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemberEntry {
    pub owner_class_id: i32,
    pub name: StrId,
    pub return_type: StrId,
    pub detail: StrId,
    pub file_path_id: u32,
    pub line: u32,
    pub kind: MemberKind,
    pub flags: u8,
    pub _pad: [u8; 2],
}

impl MemberEntry {
    pub fn access(self) -> MemberAccess {
        match self.flags & FLAG_ACCESS_MASK {
            FLAG_ACCESS_PROTECTED => MemberAccess::Protected,
            FLAG_ACCESS_PRIVATE => MemberAccess::Private,
            FLAG_ACCESS_IMPL => MemberAccess::Impl,
            _ => MemberAccess::Public,
        }
    }

    pub fn is_static(self) -> bool {
        self.flags & FLAG_STATIC != 0
    }

    pub fn line_number(self) -> Option<usize> {
        (self.line != 0).then_some(self.line as usize)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EnumValueEntry {
    pub owner_class_id: i32,
    pub name: StrId,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ParentEntry {
    pub parent_class_id: i32,
    pub parent_name: StrId,
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
        self.pool.strings.len()
            + self.file_paths.len()
            + self.class_names.len()
            + self.members.len()
            + self.member_ranges.len()
            + self.enum_values.len()
            + self.enum_ranges.len()
            + self.parents.len()
            + self.parent_ranges.len()
            + self.class_ids_by_name.len()
    }

    pub fn class_ids_by_name(&self, class_name: &str) -> Vec<i64> {
        let class_name = clean_type(class_name);
        if class_name.is_empty() {
            return Vec::new();
        }

        if let Some(ids) = self.class_ids_by_name.get(&class_name) {
            return ids.iter().map(|id| *id as i64).collect();
        }

        if class_name.contains("::") {
            let short = class_name.rsplit("::").next().unwrap_or(&class_name);
            if let Some(ids) = self.class_ids_by_name.get(short) {
                return ids.iter().map(|id| *id as i64).collect();
            }
        }

        Vec::new()
    }

    pub fn class_name_by_id(&self, class_id: i64) -> Option<&str> {
        let index = usize::try_from(class_id).ok()?;
        self.class_names
            .get(index)
            .copied()
            .and_then(|id| self.pool.get(id))
    }

    pub fn member_name<'a>(&'a self, entry: &MemberEntry) -> &'a str {
        self.pool.get(entry.name).unwrap_or("")
    }

    pub fn member_return_type<'a>(&'a self, entry: &MemberEntry) -> Option<&'a str> {
        self.pool.get(entry.return_type)
    }

    pub fn member_detail<'a>(&'a self, entry: &MemberEntry) -> Option<&'a str> {
        self.pool.get(entry.detail)
    }

    pub fn member_owner_class_name<'a>(&'a self, entry: &MemberEntry) -> Option<&'a str> {
        self.class_name_by_id(entry.owner_class_id as i64)
    }

    pub fn member_file_path<'a>(&'a self, entry: &MemberEntry) -> Option<&'a str> {
        self.file_paths
            .get(entry.file_path_id as usize)
            .copied()
            .and_then(|id| self.pool.get(id))
    }

    pub fn enum_value_name<'a>(&'a self, entry: &EnumValueEntry) -> &'a str {
        self.pool.get(entry.name).unwrap_or("")
    }

    pub fn parent_classes(&self, class_id: i64) -> &[ParentEntry] {
        let Some(class_id) = i32::try_from(class_id).ok() else {
            return &[];
        };

        slice_for_class(&self.parent_ranges, &self.parents, class_id)
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
                .filter_map(|class_id| i32::try_from(*class_id).ok())
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

            if let Some(name_id) = self.class_name_id(class_id) {
                visited_names.insert(name_id);
            }

            for entry in self.members_of_class(class_id) {
                if !include_impl_members && entry.access() == MemberAccess::Impl {
                    continue;
                }

                items.push(HotMemberItem::Member { entry, class_rank });
                if items.len() >= limit {
                    return items;
                }
            }

            for entry in self.enum_values_of_class(class_id) {
                items.push(HotMemberItem::EnumValue { entry, class_rank });
                if items.len() >= limit {
                    return items;
                }
            }

            for parent in slice_for_class(&self.parent_ranges, &self.parents, class_id) {
                if !parent.parent_name.is_none() && !visited_names.insert(parent.parent_name) {
                    continue;
                }

                if parent.parent_class_id != 0 {
                    queue.push_back((parent.parent_class_id, class_rank + 1));
                }

                if let Some(parent_name) = self.pool.get(parent.parent_name) {
                    for id in self.class_ids_by_name(parent_name) {
                        if let Ok(parent_id) = i32::try_from(id) {
                            queue.push_back((parent_id, class_rank + 1));
                        }
                    }
                }
            }
        }

        items
    }

    pub fn is_subclass_of(&self, child: &str, parent: &str) -> bool {
        let child = clean_type(child);
        let parent = clean_type(parent);

        if child == parent {
            return true;
        }

        let parent_ids = self
            .class_ids_by_name(&parent)
            .into_iter()
            .filter_map(|id| i32::try_from(id).ok())
            .collect::<HashSet<_>>();
        if parent_ids.is_empty() {
            return false;
        }

        let mut queue = VecDeque::from(
            self.class_ids_by_name(&child)
                .into_iter()
                .filter_map(|id| i32::try_from(id).ok())
                .collect::<Vec<_>>(),
        );
        let mut visited = HashSet::new();

        while let Some(class_id) = queue.pop_front() {
            if parent_ids.contains(&class_id) {
                return true;
            }

            if !visited.insert(class_id) {
                continue;
            }

            for parent_entry in slice_for_class(&self.parent_ranges, &self.parents, class_id) {
                if let Some(parent_name) = self.pool.get(parent_entry.parent_name) {
                    if clean_type(parent_name) == parent {
                        return true;
                    }

                    for id in self.class_ids_by_name(parent_name) {
                        if let Ok(parent_id) = i32::try_from(id) {
                            queue.push_back(parent_id);
                        }
                    }
                }

                if parent_entry.parent_class_id != 0 {
                    queue.push_back(parent_entry.parent_class_id);
                }
            }
        }

        false
    }

    pub fn is_member_accessible(
        &self,
        owner_class_id: i32,
        accessor_class: &str,
        flags: u8,
        assume_subclass_access: bool,
    ) -> bool {
        let access = match flags & FLAG_ACCESS_MASK {
            FLAG_ACCESS_PROTECTED => MemberAccess::Protected,
            FLAG_ACCESS_PRIVATE => MemberAccess::Private,
            FLAG_ACCESS_IMPL => MemberAccess::Impl,
            _ => MemberAccess::Public,
        };

        if accessor_class.is_empty() {
            return matches!(access, MemberAccess::Public);
        }

        let owner_class = self.class_name_by_id(owner_class_id as i64).unwrap_or("");
        if accessor_class == owner_class {
            return true;
        }

        match access {
            MemberAccess::Private => false,
            MemberAccess::Protected => {
                if assume_subclass_access {
                    true
                } else {
                    self.is_subclass_of(accessor_class, owner_class)
                }
            }
            _ => true,
        }
    }

    fn class_name_id(&self, class_id: i32) -> Option<StrId> {
        self.class_names.get(class_id as usize).copied().filter(|id| !id.is_none())
    }

    fn members_of_class(&self, class_id: i32) -> &[MemberEntry] {
        slice_for_class(&self.member_ranges, &self.members, class_id)
    }

    fn enum_values_of_class(&self, class_id: i32) -> &[EnumValueEntry] {
        slice_for_class(&self.enum_ranges, &self.enum_values, class_id)
    }
}

pub fn build_member_hot_index(conn: &Connection) -> Result<MemberHotIndex> {
    let mut pool = StringPool::new();
    let mut class_names = vec![StrId::NONE];
    let mut class_ids_by_name = HashMap::<String, Vec<i32>>::new();

    let mut stmt = conn.prepare(
        r#"
        SELECT c.id, sn.text
        FROM classes c
        JOIN strings sn ON c.name_id = sn.id
        ORDER BY sn.text, c.line_number
        "#,
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)? as i32, row.get::<_, String>(1)?)))?;
    for row in rows {
        let (class_id, class_name) = row?;
        ensure_slot(&mut class_names, class_id as usize);
        class_names[class_id as usize] = pool.intern(&class_name);
        class_ids_by_name.entry(class_name).or_default().push(class_id);
    }

    let mut file_paths = vec![StrId::NONE];
    let mut members = Vec::<MemberEntry>::new();
    let mut member_ranges = Vec::<MemberRange>::new();
    let mut current_member_class = None;
    let mut current_member_start = 0u32;

    let mut stmt = conn.prepare(
        r#"
        SELECT
            m.class_id,
            smn.text,
            smt.text,
            srt.text,
            m.access,
            m.detail,
            m.is_static,
            m.line_number,
            m.file_id,
            CASE
                WHEN dp.full_path IS NULL OR sn.text IS NULL THEN NULL
                WHEN dp.full_path = '/' THEN '/' || sn.text
                WHEN substr(dp.full_path, -1) = '/' THEN dp.full_path || sn.text
                ELSE dp.full_path || '/' || sn.text
            END
        FROM members m
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
        Ok((
            row.get::<_, i64>(0)? as i32,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
            row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u32,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<String>>(9)?,
        ))
    })?;
    for row in rows {
        let (
            owner_class_id,
            name,
            member_type,
            return_type,
            access,
            detail,
            is_static,
            line,
            file_id,
            file_path,
        ) = row?;

        if current_member_class != Some(owner_class_id) {
            push_range(&mut member_ranges, current_member_class, current_member_start, members.len() as u32);
            current_member_class = Some(owner_class_id);
            current_member_start = members.len() as u32;
        }

        let file_path_id = file_id.filter(|id| *id > 0).map(|id| id as u32).unwrap_or(0);
        if let (Some(file_id), Some(file_path)) = (file_id, file_path) {
            let file_index = file_id as usize;
            ensure_slot(&mut file_paths, file_index);
            file_paths[file_index] = pool.intern(&file_path.replace('\\', "/"));
        }

        members.push(MemberEntry {
            owner_class_id,
            name: pool.intern(&name),
            return_type: intern_optional(&mut pool, return_type.as_deref()),
            detail: intern_optional(&mut pool, detail.as_deref()),
            file_path_id,
            line,
            kind: MemberKind::from_text(&member_type),
            flags: encode_flags(is_static, access.as_deref()),
            _pad: [0; 2],
        });
    }
    push_range(&mut member_ranges, current_member_class, current_member_start, members.len() as u32);

    let mut enum_values = Vec::<EnumValueEntry>::new();
    let mut enum_ranges = Vec::<MemberRange>::new();
    let mut current_enum_class = None;
    let mut current_enum_start = 0u32;

    let mut stmt = conn.prepare(
        r#"
        SELECT ev.enum_id, sen.text
        FROM enum_values ev
        JOIN strings sen ON ev.name_id = sen.id
        ORDER BY ev.enum_id, sen.text
        "#,
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)? as i32, row.get::<_, String>(1)?)))?;
    for row in rows {
        let (owner_class_id, name) = row?;
        if current_enum_class != Some(owner_class_id) {
            push_range(&mut enum_ranges, current_enum_class, current_enum_start, enum_values.len() as u32);
            current_enum_class = Some(owner_class_id);
            current_enum_start = enum_values.len() as u32;
        }

        enum_values.push(EnumValueEntry {
            owner_class_id,
            name: pool.intern(&name),
        });
    }
    push_range(&mut enum_ranges, current_enum_class, current_enum_start, enum_values.len() as u32);

    let mut parents = Vec::<ParentEntry>::new();
    let mut parent_ranges = Vec::<MemberRange>::new();
    let mut current_parent_class = None;
    let mut current_parent_start = 0u32;

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
            row.get::<_, i64>(0)? as i32,
            row.get::<_, Option<i64>>(1)?.unwrap_or(0) as i32,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (child_id, parent_class_id, parent_name) = row?;
        if current_parent_class != Some(child_id) {
            push_range(&mut parent_ranges, current_parent_class, current_parent_start, parents.len() as u32);
            current_parent_class = Some(child_id);
            current_parent_start = parents.len() as u32;
        }

        parents.push(ParentEntry {
            parent_class_id,
            parent_name: pool.intern(&parent_name),
        });
    }
    push_range(&mut parent_ranges, current_parent_class, current_parent_start, parents.len() as u32);

    pool.clear_lookup();

    Ok(MemberHotIndex {
        pool,
        file_paths,
        class_names,
        members,
        member_ranges,
        enum_values,
        enum_ranges,
        parents,
        parent_ranges,
        class_ids_by_name,
    })
}

fn slice_for_class<'a, T>(ranges: &[MemberRange], items: &'a [T], class_id: i32) -> &'a [T] {
    let Ok(index) = ranges.binary_search_by_key(&class_id, |range| range.class_id) else {
        return &[];
    };

    let range = ranges[index];
    &items[range.start as usize..(range.start + range.len) as usize]
}

fn ensure_slot(vec: &mut Vec<StrId>, index: usize) {
    if vec.len() <= index {
        vec.resize(index + 1, StrId::NONE);
    }
}

fn push_range(ranges: &mut Vec<MemberRange>, class_id: Option<i32>, start: u32, end: u32) {
    if let Some(class_id) = class_id {
        let len = end.saturating_sub(start);
        if len != 0 {
            ranges.push(MemberRange { class_id, start, len });
        }
    }
}

fn intern_optional(pool: &mut StringPool, text: Option<&str>) -> StrId {
    text.map(|text| pool.intern(text)).unwrap_or(StrId::NONE)
}

fn encode_flags(is_static: bool, access: Option<&str>) -> u8 {
    let mut flags = if is_static { FLAG_STATIC } else { 0 };
    flags |= match access.map(str::trim).filter(|text| !text.is_empty()) {
        Some("protected") => FLAG_ACCESS_PROTECTED,
        Some("private") => FLAG_ACCESS_PRIVATE,
        Some("impl") => FLAG_ACCESS_IMPL,
        _ => FLAG_ACCESS_PUBLIC,
    };
    flags
}

fn clean_type(input: &str) -> String {
    let mut text = input
        .trim()
        .trim_start_matches("class ")
        .trim_start_matches("struct ")
        .trim();

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

    fn insert_string(conn: &Connection, text: &str) -> i64 {
        conn.execute("INSERT OR IGNORE INTO strings (text) VALUES (?1)", [text])
            .unwrap();
        conn.query_row("SELECT id FROM strings WHERE text = ?1", [text], |row| row.get(0))
            .unwrap()
    }

    fn insert_file(conn: &Connection, components: &[&str], filename: &str) -> i64 {
        let mut parent_id = None;
        for component in components {
            let name_id = insert_string(conn, component);
            conn.execute(
                "INSERT INTO directories (parent_id, name_id) VALUES (?1, ?2)",
                params![parent_id, name_id],
            )
            .unwrap();
            parent_id = Some(conn.last_insert_rowid());
        }

        let filename_id = insert_string(conn, filename);
        conn.execute(
            "INSERT INTO files (directory_id, filename_id, extension, is_header) VALUES (?1, ?2, 'h', 1)",
            params![parent_id, filename_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn member_hot_index_collects_inherited_members() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO strings (text) VALUES ('UBase'), ('UChild'), ('ParentFn'), ('ChildFn'), ('function')",
            [],
        )
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
                HotMemberItem::Member { entry, .. } => Some(index.member_name(entry)),
                HotMemberItem::EnumValue { .. } => None,
            })
            .collect::<Vec<_>>();

        assert!(names.contains(&"ChildFn"));
        assert!(names.contains(&"ParentFn"));
    }

    #[test]
    fn member_hot_index_decodes_flags_and_file_paths() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();

        let class_name = insert_string(&conn, "UTest");
        let function_type = insert_string(&conn, "function");
        let visible_name = insert_string(&conn, "VisibleFn");
        let hidden_name = insert_string(&conn, "HiddenFn");
        let return_type = insert_string(&conn, "void");
        let file_id = insert_file(&conn, &["/", "Project", "Source"], "UTest.h");

        conn.execute(
            "INSERT INTO classes (name_id, file_id, line_number, symbol_type) VALUES (?1, ?2, 1, 'class')",
            params![class_name, file_id],
        )
        .unwrap();
        let class_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO members (class_id, name_id, type_id, access, detail, return_type_id, is_static, line_number, file_id)
             VALUES (?1, ?2, ?3, 'protected', '(int32 Value)', ?4, 1, 42, ?5)",
            params![class_id, visible_name, function_type, return_type, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (class_id, name_id, type_id, access, line_number)
             VALUES (?1, ?2, ?3, 'impl', 43)",
            params![class_id, hidden_name, function_type],
        )
        .unwrap();

        let index = build_member_hot_index(&conn).unwrap();
        let items = index.collect_members_recursive("UTest", true, 128);
        let visible = items
            .iter()
            .find_map(|item| match item {
                HotMemberItem::Member { entry, .. } if index.member_name(entry) == "VisibleFn" => Some(*entry),
                _ => None,
            })
            .unwrap();

        assert_eq!(visible.access(), MemberAccess::Protected);
        assert!(visible.is_static());
        assert_eq!(visible.line_number(), Some(42));
        assert_eq!(index.member_owner_class_name(&visible), Some("UTest"));
        assert_eq!(index.member_return_type(&visible), Some("void"));
        assert_eq!(index.member_detail(&visible), Some("(int32 Value)"));
        assert_eq!(index.member_file_path(&visible), Some("/Project/Source/UTest.h"));

        let filtered = index.collect_members_recursive("UTest", false, 128);
        let filtered_names = filtered
            .iter()
            .filter_map(|item| match item {
                HotMemberItem::Member { entry, .. } => Some(index.member_name(entry)),
                HotMemberItem::EnumValue { .. } => None,
            })
            .collect::<Vec<_>>();

        assert!(filtered_names.contains(&"VisibleFn"));
        assert!(!filtered_names.contains(&"HiddenFn"));
    }
}
