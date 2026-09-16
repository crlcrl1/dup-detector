use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::model::{Token, TokenKind};

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<Xxh3>>;

const TAG_MASK: u64 = 0b11 << 62;
const FIXED_TAG: u64 = 0b00 << 62;
const IDENTIFIER_TAG: u64 = 0b01 << 62;
const LITERAL_TAG: u64 = 0b10 << 62;

pub fn token_hashes(text: &str, tokens: &[Token]) -> Vec<u64> {
    tokens
        .iter()
        .map(|token| xxh3_64(&text.as_bytes()[token.start as usize..token.end as usize]))
        .collect()
}

pub fn window_signatures(
    tokens: &[Token],
    hashes: &[u64],
    window: usize,
    parameterize_literals: bool,
) -> Vec<u64> {
    if window == 0 || tokens.len() < window {
        return Vec::new();
    }
    let total = tokens.len();
    let mut last_position: FastMap<(u64, TokenKind), u32> = FastMap::default();
    let mut previous = vec![u32::MAX; total];
    for (index, token) in tokens.iter().enumerate() {
        let key = (hashes[index], token.kind);
        if let Some(&position) = last_position.get(&key) {
            previous[index] = position;
        }
        last_position.insert(key, index as u32);
    }
    let mut bytes = vec![0u8; window * 8];
    let mut signatures = Vec::with_capacity(total - window + 1);
    for start in 0..=total - window {
        for i in 0..window {
            let index = start + i;
            let value = match tokens[index].kind {
                TokenKind::Fixed => (hashes[index] & !TAG_MASK) | FIXED_TAG,
                TokenKind::Identifier => {
                    parameterized(previous[index], start, index, IDENTIFIER_TAG)
                }
                TokenKind::Literal if parameterize_literals => {
                    parameterized(previous[index], start, index, LITERAL_TAG)
                }
                TokenKind::Literal => (hashes[index] & !TAG_MASK) | LITERAL_TAG,
            };
            bytes[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        signatures.push(xxh3_64(&bytes));
    }
    signatures
}

fn parameterized(previous: u32, start: usize, index: usize, tag: u64) -> u64 {
    let distance = if previous != u32::MAX && previous as usize >= start {
        (index - previous as usize) as u64
    } else {
        0
    };
    (distance & !TAG_MASK) | tag
}

#[allow(clippy::too_many_arguments)]
pub fn spans_equal(
    scratch: &mut SpanScratch,
    tokens_a: &[Token],
    hashes_a: &[u64],
    start_a: usize,
    tokens_b: &[Token],
    hashes_b: &[u64],
    start_b: usize,
    len: usize,
    parameterize_literals: bool,
) -> bool {
    let previous_a = &mut scratch.forward;
    let previous_b = &mut scratch.reverse;
    previous_a.clear();
    previous_b.clear();
    for i in 0..len {
        let index_a = start_a + i;
        let index_b = start_b + i;
        let token_a = &tokens_a[index_a];
        if token_a.kind != tokens_b[index_b].kind {
            return false;
        }
        match token_a.kind {
            TokenKind::Fixed => {
                if hashes_a[index_a] != hashes_b[index_b] {
                    return false;
                }
            }
            TokenKind::Identifier => {
                let distance_a = previous_a.insert(identity(token_a.kind, hashes_a[index_a]), i);
                let distance_b =
                    previous_b.insert(identity(tokens_b[index_b].kind, hashes_b[index_b]), i);
                if previous_distance(distance_a, i) != previous_distance(distance_b, i) {
                    return false;
                }
            }
            TokenKind::Literal => {
                if parameterize_literals {
                    let distance_a =
                        previous_a.insert(identity(token_a.kind, hashes_a[index_a]), i);
                    let distance_b =
                        previous_b.insert(identity(tokens_b[index_b].kind, hashes_b[index_b]), i);
                    if previous_distance(distance_a, i) != previous_distance(distance_b, i) {
                        return false;
                    }
                } else if hashes_a[index_a] != hashes_b[index_b] {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Default)]
pub struct SpanScratch {
    positions: FastMap<u64, u32>,
    forward: FastMap<u64, usize>,
    reverse: FastMap<u64, usize>,
}

impl SpanScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn span_fingerprint(
    scratch: &mut SpanScratch,
    tokens: &[Token],
    hashes: &[u64],
    start: usize,
    len: usize,
    parameterize_literals: bool,
) -> u64 {
    scratch.positions.clear();
    let mut fingerprint = 0u64;
    for i in 0..len {
        let index = start + i;
        let kind = tokens[index].kind;
        let value = match kind {
            TokenKind::Fixed => (hashes[index] & !TAG_MASK) | FIXED_TAG,
            TokenKind::Identifier => {
                fingerprint_value(scratch, identity(kind, hashes[index]), i, IDENTIFIER_TAG)
            }
            TokenKind::Literal if parameterize_literals => {
                fingerprint_value(scratch, identity(kind, hashes[index]), i, LITERAL_TAG)
            }
            TokenKind::Literal => (hashes[index] & !TAG_MASK) | LITERAL_TAG,
        };
        fingerprint = fingerprint
            .rotate_left(11)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ value;
    }
    fingerprint
}

fn fingerprint_value(scratch: &mut SpanScratch, key: u64, index: usize, tag: u64) -> u64 {
    let distance = match scratch.positions.insert(key, index as u32) {
        Some(previous) => (index - previous as usize) as u64,
        None => 0,
    };
    (distance & !TAG_MASK) | tag
}

fn identity(kind: TokenKind, hash: u64) -> u64 {
    match kind {
        TokenKind::Fixed => (hash & !TAG_MASK) | FIXED_TAG,
        TokenKind::Identifier => (hash & !TAG_MASK) | IDENTIFIER_TAG,
        TokenKind::Literal => (hash & !TAG_MASK) | LITERAL_TAG,
    }
}

fn previous_distance(previous: Option<usize>, index: usize) -> Option<usize> {
    previous.map(|position| index - position)
}

#[cfg(test)]
mod tests {
    use crate::language::LanguageId;
    use crate::tokenize::tokenize;

    use super::*;

    fn hashes(source: &str) -> (Vec<Token>, Vec<u64>) {
        let tokens = tokenize(source, LanguageId::Rust).unwrap();
        let hashes = token_hashes(source, &tokens);
        (tokens, hashes)
    }

    fn signature(source: &str, start: usize, window: usize) -> u64 {
        let (tokens, hashes) = hashes(source);
        window_signatures(&tokens, &hashes, window, false)[start]
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
        let param = |src: &str| {
            let (tokens, hashes) = hashes(src);
            window_signatures(&tokens, &hashes, 8, true)[0]
        };
        assert_eq!(param(a), param(b));
    }

    #[test]
    fn span_fingerprints_follow_encodings() {
        let (tokens_a, hashes_a) = hashes("let alpha = beta + 1;");
        let (tokens_b, hashes_b) = hashes("let gamma = delta + 1;");
        let (tokens_c, hashes_c) = hashes("let gamma = delta + 2;");
        let mut scratch = SpanScratch::new();
        let a = span_fingerprint(&mut scratch, &tokens_a, &hashes_a, 0, tokens_a.len(), false);
        let b = span_fingerprint(&mut scratch, &tokens_b, &hashes_b, 0, tokens_b.len(), false);
        let c = span_fingerprint(&mut scratch, &tokens_c, &hashes_c, 0, tokens_c.len(), false);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let a_parameterized =
            span_fingerprint(&mut scratch, &tokens_a, &hashes_a, 0, tokens_a.len(), true);
        let c_parameterized =
            span_fingerprint(&mut scratch, &tokens_c, &hashes_c, 0, tokens_c.len(), true);
        assert_eq!(a_parameterized, c_parameterized);
    }

    #[test]
    fn spans_equal_handles_renames_and_literals() {
        let a = "let alpha = beta + 1;";
        let b = "let gamma = delta + 1;";
        let (tokens_a, hashes_a) = hashes(a);
        let (tokens_b, hashes_b) = hashes(b);
        let len = tokens_a.len().min(tokens_b.len());
        let mut scratch = SpanScratch::new();
        assert!(spans_equal(
            &mut scratch,
            &tokens_a,
            &hashes_a,
            0,
            &tokens_b,
            &hashes_b,
            0,
            len,
            false
        ));
        let c = "let gamma = delta + 2;";
        let (tokens_c, hashes_c) = hashes(c);
        assert!(!spans_equal(
            &mut scratch,
            &tokens_a,
            &hashes_a,
            0,
            &tokens_c,
            &hashes_c,
            0,
            len,
            false
        ));
        assert!(spans_equal(
            &mut scratch,
            &tokens_a,
            &hashes_a,
            0,
            &tokens_c,
            &hashes_c,
            0,
            len,
            true
        ));
    }
}
