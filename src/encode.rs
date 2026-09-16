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

pub fn window_signature(
    tokens: &[Token],
    hashes: &[u64],
    start: usize,
    window: usize,
    parameterize_literals: bool,
) -> u64 {
    let mut hasher = Xxh3::new();
    for i in 0..window {
        hasher.update(&token_value(tokens, hashes, start, i, parameterize_literals).to_le_bytes());
    }
    hasher.digest()
}

#[allow(clippy::too_many_arguments)]
pub fn spans_equal(
    tokens_a: &[Token],
    hashes_a: &[u64],
    start_a: usize,
    tokens_b: &[Token],
    hashes_b: &[u64],
    start_b: usize,
    len: usize,
    parameterize_literals: bool,
) -> bool {
    let mut previous_a: FastMap<u64, usize> = FastMap::default();
    let mut previous_b: FastMap<u64, usize> = FastMap::default();
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

fn token_value(
    tokens: &[Token],
    hashes: &[u64],
    start: usize,
    i: usize,
    parameterize_literals: bool,
) -> u64 {
    let index = start + i;
    match tokens[index].kind {
        TokenKind::Fixed => (hashes[index] & !TAG_MASK) | FIXED_TAG,
        TokenKind::Identifier => {
            (distance_in_window(tokens, hashes, start, i) & !TAG_MASK) | IDENTIFIER_TAG
        }
        TokenKind::Literal => {
            if parameterize_literals {
                (distance_in_window(tokens, hashes, start, i) & !TAG_MASK) | LITERAL_TAG
            } else {
                (hashes[index] & !TAG_MASK) | LITERAL_TAG
            }
        }
    }
}

fn distance_in_window(tokens: &[Token], hashes: &[u64], start: usize, i: usize) -> u64 {
    let index = start + i;
    let kind = tokens[index].kind;
    let hash = hashes[index];
    for j in (0..i).rev() {
        if tokens[start + j].kind == kind && hashes[start + j] == hash {
            return (i - j) as u64;
        }
    }
    0
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
        window_signature(&tokens, &hashes, start, window, false)
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
            window_signature(&tokens, &hashes, 0, 8, true)
        };
        assert_eq!(param(a), param(b));
    }

    #[test]
    fn spans_equal_handles_renames_and_literals() {
        let a = "let alpha = beta + 1;";
        let b = "let gamma = delta + 1;";
        let (tokens_a, hashes_a) = hashes(a);
        let (tokens_b, hashes_b) = hashes(b);
        let len = tokens_a.len().min(tokens_b.len());
        assert!(spans_equal(
            &tokens_a, &hashes_a, 0, &tokens_b, &hashes_b, 0, len, false
        ));
        let c = "let gamma = delta + 2;";
        let (tokens_c, hashes_c) = hashes(c);
        assert!(!spans_equal(
            &tokens_a, &hashes_a, 0, &tokens_c, &hashes_c, 0, len, false
        ));
        assert!(spans_equal(
            &tokens_a, &hashes_a, 0, &tokens_c, &hashes_c, 0, len, true
        ));
    }
}
