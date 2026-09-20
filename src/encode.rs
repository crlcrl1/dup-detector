use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use xxhash_rust::xxh3::xxh3_64;

use crate::model::{Token, TokenKind};

#[derive(Default, Clone, Copy)]
pub struct FastHasher(u64);

impl FastHasher {
    #[inline]
    fn mix(&mut self, value: u64) {
        self.0 = (self.0 ^ value).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        x
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.mix(xxh3_64(bytes));
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }

    #[inline]
    fn write_u128(&mut self, n: u128) {
        self.mix(n as u64);
        self.mix((n >> 64) as u64);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_i8(&mut self, n: i8) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_i16(&mut self, n: i16) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_i32(&mut self, n: i32) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_i64(&mut self, n: i64) {
        self.mix(n as u64);
    }

    #[inline]
    fn write_i128(&mut self, n: i128) {
        self.write_u128(n as u128);
    }

    #[inline]
    fn write_isize(&mut self, n: isize) {
        self.mix(n as u64);
    }
}

pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;
pub(crate) type FastSet<T> = std::collections::HashSet<T, BuildHasherDefault<FastHasher>>;

const TAG_MASK: u64 = 0b11 << 62;
pub(crate) const FIXED_TAG: u64 = 0b00 << 62;
pub(crate) const IDENTIFIER_TAG: u64 = 0b01 << 62;
const LITERAL_TAG: u64 = 0b10 << 62;
const PARAM_TAG: u64 = 0b11 << 62;
pub(crate) const FIXED_BITS: u64 = 0b00;
pub(crate) const IDENTIFIER_BITS: u64 = 0b01;
pub(crate) const LITERAL_BITS: u64 = 0b10;

fn kind_tag(kind: u8) -> u64 {
    match TokenKind::from_u8(kind) {
        Some(TokenKind::Identifier) => IDENTIFIER_TAG,
        Some(TokenKind::Literal) => LITERAL_TAG,
        _ => FIXED_TAG,
    }
}

pub fn token_hashes(text: &str, tokens: &[Token]) -> Vec<u64> {
    tokens
        .iter()
        .map(|token| {
            let raw = xxh3_64(&text.as_bytes()[token.start as usize..token.end as usize]);
            (raw & !TAG_MASK) | kind_tag(token.kind)
        })
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
    let total = hashes.len();
    let base: Vec<u64> = hashes
        .iter()
        .map(|&hash| {
            let tag = hash >> 62;
            if tag == IDENTIFIER_BITS || (tag == LITERAL_BITS && parameterize_literals) {
                PARAM_TAG
            } else {
                hash
            }
        })
        .collect();
    let mut last_position: FastMap<u64, u32> = FastMap::default();
    let mut previous = vec![u32::MAX; total];
    for (index, &key) in hashes.iter().enumerate() {
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
            let value = if base[index] != PARAM_TAG {
                base[index]
            } else {
                parameterized(previous[index], start, index, hashes[index] & TAG_MASK)
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

pub fn spans_equal(
    scratch: &mut SpanScratch,
    hashes_a: &[u64],
    start_a: usize,
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
        let hash_a = hashes_a[start_a + i];
        let hash_b = hashes_b[start_b + i];
        let tag = hash_a >> 62;
        if tag != hash_b >> 62 {
            return false;
        }
        if tag == FIXED_BITS {
            if hash_a != hash_b {
                return false;
            }
        } else if tag == IDENTIFIER_BITS || parameterize_literals {
            let distance_a = previous_a.insert(hash_a, i);
            let distance_b = previous_b.insert(hash_b, i);
            if previous_distance(distance_a, i) != previous_distance(distance_b, i) {
                return false;
            }
        } else if hash_a != hash_b {
            return false;
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
    hashes: &[u64],
    start: usize,
    len: usize,
    parameterize_literals: bool,
) -> u64 {
    scratch.positions.clear();
    let mut fingerprint = 0u64;
    for i in 0..len {
        let hash = hashes[start + i];
        let tag = hash >> 62;
        let value = if tag == IDENTIFIER_BITS {
            fingerprint_value(scratch, hash, i, IDENTIFIER_TAG)
        } else if tag == LITERAL_BITS && parameterize_literals {
            fingerprint_value(scratch, hash, i, LITERAL_TAG)
        } else {
            hash
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

    fn window_signatures_naive(
        hashes: &[u64],
        window: usize,
        parameterize_literals: bool,
    ) -> Vec<u64> {
        let total = hashes.len();
        let base: Vec<u64> = hashes
            .iter()
            .map(|&hash| {
                let tag = hash >> 62;
                if tag == IDENTIFIER_BITS || (tag == LITERAL_BITS && parameterize_literals) {
                    PARAM_TAG
                } else {
                    hash
                }
            })
            .collect();
        let mut last_position: FastMap<u64, u32> = FastMap::default();
        let mut previous = vec![u32::MAX; total];
        for (index, &key) in hashes.iter().enumerate() {
            if let Some(&position) = last_position.get(&key) {
                previous[index] = position;
            }
            last_position.insert(key, index as u32);
        }
        let mut bytes = vec![0u8; window * 8];
        let mut signatures = Vec::new();
        for start in 0..=total - window {
            for i in 0..window {
                let index = start + i;
                let value = if base[index] != PARAM_TAG {
                    base[index]
                } else {
                    parameterized(previous[index], start, index, hashes[index] & TAG_MASK)
                };
                bytes[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            signatures.push(xxh3_64(&bytes));
        }
        signatures
    }

    #[test]
    fn sliding_window_matches_naive() {
        let sources = [
            "let alpha = beta + gamma; x",
            "fn f(a: u64) -> u64 { a.wrapping_add(1) }",
            "let x = 1; let y = 1; let z = x + y; z",
            "let a = b; let c = b; let d = b; d",
            "fn main() { let s = \"txt\"; println!(\"{s}\"); }",
        ];
        for source in sources {
            let (tokens, hashes) = hashes(source);
            if tokens.len() < 2 {
                continue;
            }
            for window in 2..=tokens.len().min(8) {
                for parameterize_literals in [false, true] {
                    assert_eq!(
                        window_signatures(&tokens, &hashes, window, parameterize_literals),
                        window_signatures_naive(&hashes, window, parameterize_literals),
                        "source: {source}, window: {window}"
                    );
                }
            }
        }
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
        let a = span_fingerprint(&mut scratch, &hashes_a, 0, tokens_a.len(), false);
        let b = span_fingerprint(&mut scratch, &hashes_b, 0, tokens_b.len(), false);
        let c = span_fingerprint(&mut scratch, &hashes_c, 0, tokens_c.len(), false);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let a_parameterized = span_fingerprint(&mut scratch, &hashes_a, 0, tokens_a.len(), true);
        let c_parameterized = span_fingerprint(&mut scratch, &hashes_c, 0, tokens_c.len(), true);
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
            &hashes_a,
            0,
            &hashes_b,
            0,
            len,
            false
        ));
        let c = "let gamma = delta + 2;";
        let (_tokens_c, hashes_c) = hashes(c);
        assert!(!spans_equal(
            &mut scratch,
            &hashes_a,
            0,
            &hashes_c,
            0,
            len,
            false
        ));
        assert!(spans_equal(
            &mut scratch,
            &hashes_a,
            0,
            &hashes_c,
            0,
            len,
            true
        ));
    }
}
