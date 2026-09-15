use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use schemars::JsonSchema;

use crate::language::LanguageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Identifier,
    Literal,
    Fixed,
}

#[derive(Debug, Clone, Copy)]
pub struct Token {
    pub kind: TokenKind,
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub end_line: u32,
    pub column: u32,
    pub unit_start: bool,
    pub unit_end: bool,
    pub unit_end_of_start: u32,
    pub unit_start_of_end: u32,
    pub container_start: bool,
    pub container_end_of_start: u32,
}

pub struct SourceFile {
    pub path: PathBuf,
    pub language: LanguageId,
    pub text: String,
    pub tokens: Vec<Token>,
    pub modified: Option<SystemTime>,
    pub size: u64,
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
    #[serde(rename = "type-3")]
    Type3,
}

#[derive(Debug, Clone)]
pub struct CloneGroup {
    pub occurrences: Vec<Occurrence>,
    pub token_count: usize,
    pub similarity: f64,
    pub clone_type: CloneType,
}
