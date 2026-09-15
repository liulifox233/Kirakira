//! XP3 archive filters (`TVPSetXP3ArchiveExtractionFilter`,
//! `TVPSetXP3ArchiveContentFilter`).
//!
//! KRKR reads a filtered XP3 by installing two callbacks into the archive
//! read path; `xp3filter.dll` is the plugin that does it (K2
//! `src/plugins/xp3filter.cpp`). The reference keeps both as file-scope
//! function pointers consulted *late* — the content filter when an entry
//! stream is created (`tTVPXP3Archive::CreateStreamByIndex`,
//! `Kirikiroid2/src/core/base/XP3Archive.cpp:585-595`) and the extraction
//! filter on every chunk of every read (`tTVPXP3ArchiveStream::Read`,
//! `:1045-1053`). Neither is captured when an archive is opened, which is why
//! K2 can install the callbacks from a post-registration step even though the
//! game's archives were already mounted.
//!
//! The vocabulary lives here, next to [`crate::StorageMediaProvider`], for the
//! same reason that one does: a plugin crate names these types, and no plugin
//! may depend on `krkr-assets` or `krkr-xp3`. `krkr-xp3` reads the registry
//! when it creates an entry stream and on every read; the host storage owns
//! the instance and hands it out through
//! [`ProjectStoragePort::xp3_filter_registry`](crate::ProjectStoragePort::xp3_filter_registry).

use std::{
    any::Any,
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// `tTVPXP3ExtractionFilterInfo` (`Kirikiroid2/src/core/base/XP3Archive.h:25-40`):
/// the chunk an extraction callback may rewrite in place.
///
/// `offset` is the chunk's position in the *uncompressed* stream (`CurPos` at
/// `XP3Archive.cpp:1047`), `buffer` is the writable chunk itself, `file_hash`
/// is the entry's index hash (the `FileHash` field, "interface v2"), and
/// `file_name` is the entry's name inside the archive (`Owner->GetName(...)`,
/// `:1047-1048`).
pub struct Xp3ExtractionFilterInfo<'a> {
    pub offset: u64,
    pub buffer: &'a mut [u8],
    pub file_hash: u32,
    pub file_name: &'a str,
}

impl fmt::Debug for Xp3ExtractionFilterInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xp3ExtractionFilterInfo")
            .field("offset", &self.offset)
            .field("buffer_len", &self.buffer.len())
            .field("file_hash", &self.file_hash)
            .field("file_name", &self.file_name)
            .finish()
    }
}

/// The per-stream `tTJSVariant FilterContext` of the reference
/// (`XP3Archive.cpp:586` seeds it through the content filter, `:1054` hands
/// the same value to every extraction callback of that stream).
///
/// The reference lets the plugin put any TJS value there; this port lets it
/// put any `Send` Rust value, which is what a filter running off the script
/// thread can carry. A stream starts with an empty context.
#[derive(Default)]
pub struct Xp3FilterContext {
    state: Option<Box<dyn Any + Send>>,
}

impl Xp3FilterContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value the content filter stored, when it stored one of type `T`.
    pub fn get<T: Any + Send>(&self) -> Option<&T> {
        self.state.as_ref()?.downcast_ref::<T>()
    }

    /// [`Self::get`] for mutation: the extraction filter sees the same value.
    pub fn get_mut<T: Any + Send>(&mut self) -> Option<&mut T> {
        self.state.as_mut()?.downcast_mut::<T>()
    }

    /// Stores (or replaces) the context value.
    pub fn set<T: Any + Send>(&mut self, state: T) {
        self.state = Some(Box::new(state));
    }

    pub fn clear(&mut self) {
        self.state = None;
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_none()
    }
}

impl fmt::Debug for Xp3FilterContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xp3FilterContext")
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

/// `tTVPXP3ArchiveExtractionFilter` (`XP3Archive.h:52-53`): rewrite one chunk
/// of an entry's decompressed bytes.
///
/// The callback is consulted per chunk, so an installed filter sees every
/// read, and a filter installed after a stream was created still applies —
/// that is the reference's ordering rule, not an accident of this port.
/// Implementations must be safe to call from a resource worker.
pub trait Xp3ExtractionFilter: Send + Sync {
    fn apply(&self, info: Xp3ExtractionFilterInfo<'_>, ctx: &mut Xp3FilterContext);
}

impl<F> Xp3ExtractionFilter for F
where
    F: Fn(Xp3ExtractionFilterInfo<'_>, &mut Xp3FilterContext) + Send + Sync,
{
    fn apply(&self, info: Xp3ExtractionFilterInfo<'_>, ctx: &mut Xp3FilterContext) {
        self(info, ctx)
    }
}

/// What a content callback answers (`XP3Archive.cpp:585-595`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Xp3ContentFilterAction {
    /// `0`: read through the normal entry stream.
    Decode,
    /// `1` — `XP3_CONTENT_FILTER_FETCH_FULLDATA`: the archive reads the whole
    /// entry into memory at stream creation and the caller gets those bytes.
    FetchFull,
}

/// `tTVPXP3ArchiveContentFilter` (`XP3Archive.h:54-55`): decide how one entry
/// is read before its first byte.
///
/// `ctx` is the context of the stream being created; what the callback stores
/// there reaches every extraction call of that stream.
pub trait Xp3ContentFilter: Send + Sync {
    fn apply(
        &self,
        file_name: &str,
        archive_name: &str,
        file_size: u64,
        ctx: &mut Xp3FilterContext,
    ) -> Xp3ContentFilterAction;
}

impl<F> Xp3ContentFilter for F
where
    F: Fn(&str, &str, u64, &mut Xp3FilterContext) -> Xp3ContentFilterAction + Send + Sync,
{
    fn apply(
        &self,
        file_name: &str,
        archive_name: &str,
        file_size: u64,
        ctx: &mut Xp3FilterContext,
    ) -> Xp3ContentFilterAction {
        self(file_name, archive_name, file_size, ctx)
    }
}

/// The two filter slots an archive consults, as one shared object.
///
/// The reference keeps a process-wide function pointer per slot
/// (`XP3Archive.cpp:30-40`); a shared registry is the same thing scoped to a
/// project storage, so two engines in one process (tests, the debugger shells)
/// do not install filters into each other. Archives hold an [`Arc`] of it and
/// read the slots per stream creation and per read.
#[derive(Default)]
pub struct Xp3FilterRegistry {
    extraction: RwLock<Option<Arc<dyn Xp3ExtractionFilter>>>,
    content: RwLock<Option<Arc<dyn Xp3ContentFilter>>>,
    generation: AtomicU64,
}

impl Xp3FilterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `TVPSetXP3ArchiveExtractionFilter` (`:31-34`); `None` clears the slot
    /// the way `TVPSetXP3FilterScript("")` does (`xp3filter.cpp:429-430`).
    pub fn set_extraction_filter(&self, filter: Option<Arc<dyn Xp3ExtractionFilter>>) {
        self.write(&self.extraction, filter);
    }

    /// `TVPSetXP3ArchiveContentFilter` (`:38-41`).
    pub fn set_content_filter(&self, filter: Option<Arc<dyn Xp3ContentFilter>>) {
        self.write(&self.content, filter);
    }

    pub fn extraction_filter(&self) -> Option<Arc<dyn Xp3ExtractionFilter>> {
        self.read(&self.extraction)
    }

    pub fn content_filter(&self) -> Option<Arc<dyn Xp3ContentFilter>> {
        self.read(&self.content)
    }

    /// Bumped by every install or clear.
    ///
    /// Cache layers above the archive (this port's raw-byte cache) key their
    /// entries on it: bytes read through the previous filters must not be
    /// served after the slots moved. The reference has no such layer — it
    /// reads through the live pointers every time (`XP3Archive.cpp:1047`).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn write<T>(&self, slot: &RwLock<Option<Arc<T>>>, value: Option<Arc<T>>)
    where
        T: ?Sized,
    {
        // A poisoned lock means a filter panicked while another thread held
        // it; the slot is a plain pointer swap, so the last write still wins
        // and a recovered guard is the safe reading.
        let mut guard = slot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = value;
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn read<T>(&self, slot: &RwLock<Option<Arc<T>>>) -> Option<Arc<T>>
    where
        T: ?Sized,
    {
        slot.read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl fmt::Debug for Xp3FilterRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xp3FilterRegistry")
            .field("has_extraction_filter", &self.extraction_filter().is_some())
            .field("has_content_filter", &self.content_filter().is_some())
            .field("generation", &self.generation())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_context_carries_a_value_between_the_two_filters() {
        let mut ctx = Xp3FilterContext::new();
        assert!(ctx.is_empty());
        ctx.set(7u32);
        assert_eq!(ctx.get::<u32>(), Some(&7));
        assert_eq!(ctx.get::<u64>(), None);
        *ctx.get_mut::<u32>().expect("stored value") = 9;
        assert_eq!(ctx.get::<u32>(), Some(&9));
        ctx.clear();
        assert!(ctx.is_empty());
    }

    #[test]
    fn installing_and_clearing_filters_moves_the_generation() {
        let registry = Xp3FilterRegistry::new();
        assert_eq!(registry.generation(), 0);
        assert!(registry.extraction_filter().is_none());
        registry.set_extraction_filter(Some(Arc::new(
            |_info: Xp3ExtractionFilterInfo<'_>, _ctx: &mut Xp3FilterContext| {},
        )));
        registry.set_content_filter(Some(Arc::new(
            |_file: &str, _archive: &str, _size: u64, _ctx: &mut Xp3FilterContext| {
                Xp3ContentFilterAction::Decode
            },
        )));
        assert_eq!(registry.generation(), 2);
        assert!(registry.extraction_filter().is_some());
        assert!(registry.content_filter().is_some());
        registry.set_extraction_filter(None);
        registry.set_content_filter(None);
        assert_eq!(registry.generation(), 4);
        assert!(registry.extraction_filter().is_none());
        assert!(registry.content_filter().is_none());
    }
}
