use super::SharedCode;
use std::{collections::VecDeque, mem::size_of, sync::Arc};

/// Interactive source only. Bulk searches must not evict the user's open classes.
/// Clones share immutable documents but have independent, bounded LRU state.
#[derive(Clone)]
pub(super) struct SourceCache {
    entries: VecDeque<(String, Arc<SharedCode>, usize)>,
    bytes: usize,
    budget: usize,
    limit: usize,
}
impl Default for SourceCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget: 64 * 1024 * 1024,
            limit: 8,
        }
    }
}
impl SourceCache {
    pub(super) fn trim(&mut self, budget: usize) {
        while self.bytes > budget {
            let Some((_, _, bytes)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= bytes;
        }
    }
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(super) fn get(&mut self, class: &str) -> Option<Arc<SharedCode>> {
        let index = self.entries.iter().position(|(key, _, _)| key == class)?;
        let entry = self.entries.remove(index)?;
        let code = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(code)
    }
    pub(super) fn insert(&mut self, class: &str, code: Arc<SharedCode>) {
        if let Some(index) = self.entries.iter().position(|(key, _, _)| key == class) {
            self.bytes -= self.entries.remove(index).unwrap().2;
        }
        let bytes = class.len()
            + size_of::<SharedCode>()
            + code.source.capacity()
            + code.source_hash.capacity()
            + code.links.capacity() * size_of::<super::CodeLink>()
            + code.definitions.capacity() * size_of::<super::CodeDefinition>()
            + code
                .links
                .iter()
                .map(|link| link.label.capacity())
                .sum::<usize>()
            + code
                .definitions
                .iter()
                .map(|def| def.kind.capacity() + def.name.capacity())
                .sum::<usize>();
        if bytes > self.budget || self.limit == 0 {
            return;
        }
        while self.entries.len() >= self.limit || self.bytes + bytes > self.budget {
            self.bytes -= self.entries.pop_front().unwrap().2;
        }
        self.entries.push_back((class.to_owned(), code, bytes));
        self.bytes += bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DecompiledCode;
    fn code(text: &str) -> Arc<SharedCode> {
        Arc::new(SharedCode::from(DecompiledCode {
            source: text.into(),
            source_hash: super::super::source_identity(text),
            links: vec![],
            definitions: vec![],
        }))
    }
    #[test]
    fn cache_evicts_least_recent_and_rejects_oversized_documents() {
        let mut cache = SourceCache {
            limit: 2,
            ..Default::default()
        };
        cache.insert("a", code("a"));
        cache.insert("b", code("b"));
        assert!(cache.get("a").is_some());
        cache.insert("c", code("c"));
        assert!(cache.get("b").is_none());
        let retained = cache.bytes;
        cache.insert("huge", code(&"x".repeat(cache.budget + 1)));
        assert_eq!(cache.bytes, retained);
        cache.budget = retained;
        // SharedCode's inline size can change without changing LRU semantics.
        // Choose a payload which fits alone but cannot coexist with either entry.
        let larger = code(&"x".repeat(retained / 3));
        assert!(larger.retained_bytes() + "larger".len() <= retained);
        cache.insert("larger", larger);
        assert!(cache.bytes <= cache.budget);
        assert_eq!(cache.entries.len(), 1);
    }
    #[test]
    fn cache_clone_shares_documents_and_replacement_accounts_for_bytes() {
        let mut cache = SourceCache::default();
        cache.insert("a", code("first"));
        let mut clone = cache.clone();
        assert!(Arc::ptr_eq(
            &cache.get("a").unwrap(),
            &clone.get("a").unwrap()
        ));
        cache.insert("a", code("second"));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, cache.entries[0].2);
        assert_eq!(clone.get("a").unwrap().source.as_str(), "first");
        assert_eq!(cache.get("a").unwrap().source.as_str(), "second");
    }
}
