use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Index into a `StringPool`. 32-bit to keep entry structs compact.
/// `StringPool` 中的索引。使用 32 位以保持 entry 结构紧凑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct StrId(pub u32);

impl StrId {
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Per-hot-index string interner. Each hot index owns its own pool to keep
/// serialization self-contained.
/// 每个 hot index 各自持有一份字符串驻留池，保证序列化自洽。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StringPool {
    strings: Vec<String>,
    #[serde(skip)]
    lookup: HashMap<String, u32>,
}

impl StringPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Intern a string, returning a stable `StrId`. Empty input is allowed.
    /// 驻留一个字符串并返回稳定的 `StrId`，允许空串。
    pub fn intern(&mut self, value: &str) -> StrId {
        if let Some(&id) = self.lookup.get(value) {
            return StrId(id);
        }
        let id = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.lookup.insert(value.to_string(), id);
        StrId(id)
    }

    /// Intern when present, otherwise `None`.
    /// `Some(s)` 才驻留，`None` 透传。
    pub fn intern_opt(&mut self, value: Option<&str>) -> Option<StrId> {
        value.map(|s| self.intern(s))
    }

    /// Resolve an id back to its string.
    /// 从 id 反查字符串。
    #[inline]
    pub fn get(&self, id: StrId) -> &str {
        &self.strings[id.as_usize()]
    }

    /// Resolve an optional id.
    /// 反查可选 id。
    #[inline]
    pub fn get_opt(&self, id: Option<StrId>) -> Option<&str> {
        id.map(|i| self.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_for_same_string() {
        let mut pool = StringPool::new();
        let a = pool.intern("hello");
        let b = pool.intern("hello");
        assert_eq!(a, b);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn intern_different_strings_yields_different_ids() {
        let mut pool = StringPool::new();
        let a = pool.intern("foo");
        let b = pool.intern("bar");
        assert_ne!(a, b);
        assert_eq!(pool.get(a), "foo");
        assert_eq!(pool.get(b), "bar");
    }

    #[test]
    fn intern_opt_passes_none_through() {
        let mut pool = StringPool::new();
        assert!(pool.intern_opt(None).is_none());
        let id = pool.intern_opt(Some("x")).unwrap();
        assert_eq!(pool.get(id), "x");
    }

    #[test]
    fn serde_roundtrip_preserves_strings() {
        let mut pool = StringPool::new();
        pool.intern("alpha");
        pool.intern("beta");
        let bytes = rmp_serde::to_vec(&pool).unwrap();
        let restored: StringPool = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(restored.get(StrId(0)), "alpha");
        assert_eq!(restored.get(StrId(1)), "beta");
    }
}
