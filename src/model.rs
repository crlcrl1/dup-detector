use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use schemars::JsonSchema;

use crate::language::LanguageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Identifier,
    Literal,
    Fixed,
}

#[derive(Debug, Clone, Copy)]
pub struct Token {
    pub kind: TokenKind,
    pub flags: u8,
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub end_line: u32,
    pub unit_end_of_start: u32,
    pub unit_start_of_end: u32,
    pub container_end_of_start: u32,
}

pub const FLAG_UNIT_START: u8 = 1;
pub const FLAG_UNIT_END: u8 = 2;
pub const FLAG_CONTAINER_START: u8 = 4;

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

pub struct SourceFile {
    pub path: PathBuf,
    pub language: LanguageId,
    pub text: String,
    pub tokens: Vec<Token>,
    pub hashes: Vec<u64>,
    seeds: Mutex<Option<SeedCache>>,
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
        Self {
            path,
            language,
            text,
            tokens,
            hashes,
            seeds: Mutex::new(None),
            modified,
            size,
        }
    }

    pub fn for_each_window_signature(
        &self,
        window: usize,
        parameterize_literals: bool,
        f: impl FnOnce(&[u64]),
    ) {
        if window == 0 {
            f(&[]);
            return;
        }
        if let Ok(cache) = self.seeds.lock()
            && let Some(cached) = cache.as_ref()
            && cached.window == window
            && cached.parameterize_literals == parameterize_literals
        {
            f(&cached.signatures);
            return;
        }
        let signatures = crate::encode::window_signatures(
            &self.tokens,
            &self.hashes,
            window,
            parameterize_literals,
        );
        f(&signatures);
        if let Ok(mut cache) = self.seeds.lock() {
            *cache = Some(SeedCache {
                window,
                parameterize_literals,
                signatures,
            });
        }
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
