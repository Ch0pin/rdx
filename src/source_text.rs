//! Immutable text with an anonymous file-backed representation for inactive views.
use std::{fs::File, io::Write, ops::Deref, sync::Arc};
pub trait TextOwner: Send + Sync + std::fmt::Debug {
    fn text(&self) -> &str;
    fn heap_bytes(&self) -> usize;
}
#[derive(Clone, Debug)]
pub enum SourceText {
    Heap(Arc<String>),
    File(Arc<FileText>),
    Owner(Arc<dyn TextOwner>),
}
#[derive(Debug)]
pub struct FileText {
    // Drop the mapping before the private file handle (including on Windows).
    map: memmap2::Mmap,
    _file: File,
}
impl SourceText {
    pub fn new(text: Arc<String>) -> Self {
        Self::Heap(text)
    }
    pub fn heap_bytes(&self) -> usize {
        match self {
            Self::Heap(s) => s.capacity(),
            Self::File(_) => 0,
            Self::Owner(owner) => owner.heap_bytes(),
        }
    }
    /// Replace only after writing and mapping succeed. Failure keeps the original text.
    pub fn page_out(&mut self) -> std::io::Result<bool> {
        self.page_out_with(tempfile::tempfile)
    }
    fn page_out_with(
        &mut self,
        create: impl FnOnce() -> std::io::Result<File>,
    ) -> std::io::Result<bool> {
        if matches!(self, Self::File(_)) || self.is_empty() {
            return Ok(false);
        }
        let mut file = create()?;
        file.write_all(self.as_bytes())?;
        file.flush()?;
        // SAFETY: this anonymous, private file is owned solely here. Its handle is
        // never exposed, resized or written again; FileText retains it for the map.
        let map = unsafe { memmap2::MmapOptions::new().map(&file)? };
        *self = Self::File(Arc::new(FileText { map, _file: file }));
        Ok(true)
    }
}
impl Deref for SourceText {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Heap(text) => text,
            Self::Owner(owner) => owner.text(),
            // SAFETY: bytes were copied from a String and the private backing file
            // is immutable for the entire mapping lifetime.
            Self::File(text) => unsafe { std::str::from_utf8_unchecked(&text.map) },
        }
    }
}
impl AsRef<str> for SourceText {
    fn as_ref(&self) -> &str {
        self
    }
}
impl<T: AsRef<str>> PartialEq<T> for SourceText {
    fn eq(&self, other: &T) -> bool {
        self.as_ref() == other.as_ref()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_spill_failure_preserves_the_live_text() {
        let original = Arc::new("Café 🦀".to_owned());
        let mut text = SourceText::new(original.clone());
        let error = text.page_out_with(|| Err(std::io::Error::other("disk full")));
        assert!(error.is_err());
        assert!(matches!(&text, SourceText::Heap(s) if Arc::ptr_eq(s, &original)));
        assert_eq!(text.as_ref(), original.as_str());
    }
    #[test]
    fn file_backing_preserves_unicode_offsets_and_survives_other_owners() {
        let original = Arc::new("class Café { 🦀 }\n".repeat(10000));
        let weak = Arc::downgrade(&original);
        let mut text = SourceText::new(original.clone());
        assert!(text.page_out().unwrap());
        assert_eq!(text.as_ref(), original.as_str());
        assert_eq!(text.heap_bytes(), 0);
        let copy = text.clone();
        drop(original);
        drop(text);
        assert!(weak.upgrade().is_none());
        assert_eq!(copy.chars().filter(|c| *c == '🦀').count(), 10000);
    }
}
