use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use schemars::JsonSchema;

use crate::language::LanguageId;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Identifier,
    Literal,
    Fixed,
}

impl TokenKind {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Identifier),
            1 => Some(Self::Literal),
            2 => Some(Self::Fixed),
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Token {
    pub start: u32,
    pub end: u32,
    pub unit_end_of_start: u32,
    pub unit_start_of_end: u32,
    pub container_end_of_start: u32,
    pub kind: u8,
    pub flags: u8,
    pub _pad: [u8; 2],
}

#[derive(Clone)]
pub enum Text {
    Owned(String),
    Mapped(Arc<Mmap>),
}

impl Text {
    /// Maps file contents, validating UTF-8 up front.
    ///
    /// The unchecked UTF-8 access in [`Text::as_str`] stays sound only while
    /// the backing file is never modified in place: the file must be replaced
    /// (write-temp-then-rename) or unmapped before any in-place write. The
    /// index re-stats files on refresh and remaps changed ones, which covers
    /// truncation; an in-place rewrite preserving both size and mtime would
    /// still silently break this invariant.
    #[cfg(unix)]
    pub fn mapped(map: Arc<Mmap>) -> Option<Self> {
        std::str::from_utf8(&map).ok()?;
        Some(Self::Mapped(map))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Text::Owned(text) => text,
            // SAFETY: `mapped` validated the whole mapping as UTF-8, and the
            // backing file must not be mutated in place while alive (see the
            // contract on `mapped`).
            Text::Mapped(map) => unsafe { std::str::from_utf8_unchecked(map) },
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Text::Owned(text) => text.as_bytes(),
            Text::Mapped(map) => map,
        }
    }

    pub fn release_pages(&self) {
        #[cfg(unix)]
        if let Text::Mapped(map) = self {
            // SAFETY: the mapping is read-only, so dropping its resident pages
            // only forces later accesses to be served from the backing file.
            let _ = unsafe { map.unchecked_advise(memmap2::UncheckedAdvice::DontNeed) };
        }
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

pub enum SliceRepr<T> {
    Owned(Vec<T>),
    Mapped {
        map: Arc<Mmap>,
        offset: usize,
        len: usize,
    },
}

pub struct Slice<T> {
    repr: SliceRepr<T>,
}

impl<T> Slice<T> {
    pub fn owned(values: Vec<T>) -> Self {
        Self {
            repr: SliceRepr::Owned(values),
        }
    }
}

impl<T: bytemuck::Pod> Slice<T> {
    pub fn mapped(map: Arc<Mmap>, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len.checked_mul(size_of::<T>())?)?;
        if end > map.len() || !offset.is_multiple_of(align_of::<T>()) {
            return None;
        }
        Some(Self {
            repr: SliceRepr::Mapped { map, offset, len },
        })
    }

    pub fn as_slice(&self) -> &[T] {
        match &self.repr {
            SliceRepr::Owned(values) => values,
            // SAFETY: `mapped` validated the offset/length/alignment and the
            // mapping is never mutated while alive.
            SliceRepr::Mapped { map, offset, len } => {
                let bytes = &map[*offset..*offset + *len * size_of::<T>()];
                bytemuck::cast_slice(bytes)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_mapped(&self) -> bool {
        matches!(self.repr, SliceRepr::Mapped { .. })
    }
}

impl<T: bytemuck::Pod> Deref for Slice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

pub const FLAG_UNIT_START: u8 = 1;
pub const FLAG_UNIT_END: u8 = 2;
pub const FLAG_CONTAINER_START: u8 = 4;
pub const FLAG_BRACKET_OPEN: u8 = 8;
pub const FLAG_BRACKET_CLOSE: u8 = 16;

impl Token {
    #[inline]
    pub fn unit_start(&self) -> bool {
        self.flags & FLAG_UNIT_START != 0
    }

    #[inline]
    pub fn unit_end(&self) -> bool {
        self.flags & FLAG_UNIT_END != 0
    }

    #[inline]
    pub fn container_start(&self) -> bool {
        self.flags & FLAG_CONTAINER_START != 0
    }

    #[inline]
    pub fn bracket_open(&self) -> bool {
        self.flags & FLAG_BRACKET_OPEN != 0
    }

    #[inline]
    pub fn bracket_close(&self) -> bool {
        self.flags & FLAG_BRACKET_CLOSE != 0
    }

    #[inline]
    pub fn set_unit_start(&mut self) {
        self.flags |= FLAG_UNIT_START;
    }

    #[inline]
    pub fn set_unit_end(&mut self) {
        self.flags |= FLAG_UNIT_END;
    }

    #[inline]
    pub fn set_container_start(&mut self) {
        self.flags |= FLAG_CONTAINER_START;
    }
}

fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = Vec::with_capacity(text.len() / 32 + 1);
    starts.push(0);
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'\n')
            .map(|(index, _)| (index + 1) as u32),
    );
    starts
}

pub struct SpanMeta {
    pub(crate) pairs: Vec<u32>,
    pub(crate) balance: Vec<i32>,
    pub(crate) next_lower: Vec<u32>,
}

pub struct SourceFile {
    pub path: PathBuf,
    pub language: LanguageId,
    pub text: Text,
    pub tokens: Slice<Token>,
    pub hashes: Slice<u64>,
    line_starts: Vec<u32>,
    seeds: Mutex<Option<SeedCache>>,
    span_meta: OnceLock<Arc<SpanMeta>>,
    pub modified: Option<SystemTime>,
    pub size: u64,
}

pub struct SeedCache {
    pub window: usize,
    pub parameterize_literals: bool,
    pub signatures: Vec<u64>,
}

impl SourceFile {
    pub fn new(
        path: PathBuf,
        language: LanguageId,
        text: String,
        tokens: Vec<Token>,
        modified: Option<SystemTime>,
        size: u64,
    ) -> Self {
        let hashes = crate::encode::token_hashes(&text, &tokens);
        Self::with_hashes(path, language, text, tokens, hashes, modified, size)
    }

    pub fn with_hashes(
        path: PathBuf,
        language: LanguageId,
        text: String,
        tokens: Vec<Token>,
        hashes: Vec<u64>,
        modified: Option<SystemTime>,
        size: u64,
    ) -> Self {
        Self::from_storage(
            path,
            language,
            Text::Owned(text),
            Slice::owned(tokens),
            Slice::owned(hashes),
            modified,
            size,
        )
    }

    pub fn new_with_text(
        path: PathBuf,
        language: LanguageId,
        text: Text,
        tokens: Vec<Token>,
        modified: Option<SystemTime>,
        size: u64,
    ) -> Self {
        let hashes = crate::encode::token_hashes(text.as_str(), &tokens);
        Self::from_storage(
            path,
            language,
            text,
            Slice::owned(tokens),
            Slice::owned(hashes),
            modified,
            size,
        )
    }

    pub fn from_storage(
        path: PathBuf,
        language: LanguageId,
        text: Text,
        tokens: Slice<Token>,
        hashes: Slice<u64>,
        modified: Option<SystemTime>,
        size: u64,
    ) -> Self {
        let line_starts = line_starts(text.as_str());
        Self {
            path,
            language,
            text,
            tokens,
            hashes,
            line_starts,
            seeds: Mutex::new(None),
            span_meta: OnceLock::new(),
            modified,
            size,
        }
    }

    pub fn line_at(&self, offset: u32) -> u32 {
        self.line_starts.partition_point(|&start| start <= offset) as u32
    }

    pub fn token_line(&self, index: usize) -> u32 {
        self.line_at(self.tokens[index].start)
    }

    pub fn token_end_line(&self, index: usize) -> u32 {
        self.line_at(self.tokens[index].end.saturating_sub(1))
    }

    pub fn line_offset(&self, line: u32) -> usize {
        self.line_starts
            .get(line.saturating_sub(1) as usize)
            .copied()
            .unwrap_or(self.text.len() as u32) as usize
    }

    pub fn line_span(&self, occurrence: &Occurrence) -> usize {
        let first = self.token_line(occurrence.start as usize) as usize;
        let last = self.token_end_line(occurrence.end as usize - 1) as usize;
        last.saturating_sub(first) + 1
    }

    pub fn for_each_window_signature(
        &self,
        window: usize,
        parameterize_literals: bool,
        cache: bool,
        f: impl FnOnce(&[u64]),
    ) {
        if window == 0 {
            f(&[]);
            return;
        }
        if cache
            && let Ok(cached) = self.seeds.lock()
            && let Some(cached) = cached.as_ref()
            && cached.window == window
            && cached.parameterize_literals == parameterize_literals
        {
            f(&cached.signatures);
            return;
        }
        let signatures =
            crate::encode::window_signatures(self.hashes.as_slice(), window, parameterize_literals);
        f(&signatures);
        if cache && let Ok(mut cached) = self.seeds.lock() {
            *cached = Some(SeedCache {
                window,
                parameterize_literals,
                signatures,
            });
        }
    }

    pub(crate) fn cached_span_meta(&self, compute: impl FnOnce() -> SpanMeta) -> &Arc<SpanMeta> {
        self.span_meta.get_or_init(|| Arc::new(compute()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Occurrence {
    pub file: u32,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CloneType {
    #[serde(rename = "type-1")]
    Type1,
    #[serde(rename = "type-2")]
    Type2,
}

#[derive(Debug, Clone)]
pub struct CloneGroup {
    pub occurrences: Vec<Occurrence>,
    pub token_count: usize,
    pub clone_type: CloneType,
}
