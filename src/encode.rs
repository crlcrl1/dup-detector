use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::model::{SourceFile, Token, TokenKind};

const TAG_MASK: u64 = 0b11 << 62;
const FIXED_TAG: u64 = 0b00 << 62;
const IDENTIFIER_TAG: u64 = 0b01 << 62;
const LITERAL_TAG: u64 = 0b10 << 62;

pub fn window_signature(
    file: &SourceFile,
    start: usize,
    window: usize,
    parameterize_literals: bool,
) -> u64 {
    let mut hasher = Xxh3::new();
    for i in 0..window {
        hasher.update(&token_value(file, start, i, parameterize_literals).to_le_bytes());
    }
    hasher.digest()
}

pub fn span_keys(
    file: &SourceFile,
    start: usize,
    len: usize,
    parameterize_literals: bool,
) -> Vec<u64> {
    (0..len)
        .map(|i| token_value(file, start, i, parameterize_literals))
        .collect()
}

fn token_value(file: &SourceFile, start: usize, i: usize, parameterize_literals: bool) -> u64 {
    let tokens = &file.tokens;
    let text = &file.text;
    let token = &tokens[start + i];
    match token.kind {
        TokenKind::Fixed => {
            (xxh3_64(&text.as_bytes()[token.start as usize..token.end as usize]) & !TAG_MASK)
                | FIXED_TAG
        }
        TokenKind::Identifier => {
            (distance_in_window(tokens, text, start, i) & !TAG_MASK) | IDENTIFIER_TAG
        }
        TokenKind::Literal => {
            if parameterize_literals {
                (distance_in_window(tokens, text, start, i) & !TAG_MASK) | LITERAL_TAG
            } else {
                (xxh3_64(&text.as_bytes()[token.start as usize..token.end as usize]) & !TAG_MASK)
                    | LITERAL_TAG
            }
        }
    }
}

fn distance_in_window(tokens: &[Token], text: &str, start: usize, i: usize) -> u64 {
    let token = &tokens[start + i];
    for j in (0..i).rev() {
        let prev = &tokens[start + j];
        if prev.kind == token.kind
            && text[prev.start as usize..prev.end as usize]
                == text[token.start as usize..token.end as usize]
        {
            return (i - j) as u64;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::language::LanguageId;
    use crate::tokenize::tokenize;

    fn file(source: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.rs"),
            language: LanguageId::Rust,
            text: source.to_string(),
            tokens: tokenize(source, LanguageId::Rust).unwrap(),
            modified: None,
            size: 0,
        }
    }

    fn signature(source: &str, start: usize, window: usize) -> u64 {
        window_signature(&file(source), start, window, false)
    }

    #[test]
    fn rename_invariant() {
        let a = "let alpha = beta + gamma; x";
        let b = "let p = q + r; s";
        assert_eq!(signature(a, 0, 8), signature(b, 0, 8));
    }

    #[test]
    fn different_structure_differs() {
        let a = "let alpha = beta + gamma; x";
        let b = "let alpha = beta * gamma; x";
        assert_ne!(signature(a, 0, 8), signature(b, 0, 8));
    }

    #[test]
    fn different_identifier_layout_differs() {
        let a = "let alpha = beta + alpha; x";
        let b = "let alpha = beta + beta; x";
        assert_ne!(signature(a, 0, 8), signature(b, 0, 8));
    }

    #[test]
    fn literals_are_exact_by_default() {
        let a = "let x = 100 + 200; y";
        let b = "let x = 100 + 300; y";
        assert_ne!(signature(a, 0, 8), signature(b, 0, 8));
        let param = |src: &str| window_signature(&file(src), 0, 8, true);
        assert_eq!(param(a), param(b));
    }
}
