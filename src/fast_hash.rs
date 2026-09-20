use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use xxhash_rust::xxh3::xxh3_64;

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
