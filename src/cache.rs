use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;
use xxhash_rust::xxh3::xxh3_64;

use crate::config::{CONFIG_DIR_NAME, CacheLocation};
use crate::model::{
    FLAG_CONTAINER_START, FLAG_UNIT_END, FLAG_UNIT_START, Slice, SourceFile, Token, TokenKind,
};

const MAGIC: &[u8; 8] = b"DUPCDT13";
const HEADER_BYTES: usize = 52;

pub const CACHE_DIR: &str = ".dup-detector";

pub struct CacheEntry {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: u64,
    pub text_hash: u64,
    pub grammar_fingerprint: u64,
    pub tokens: Slice<Token>,
}

struct Header {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    text_hash: u64,
    grammar_fingerprint: u64,
    token_count: usize,
    tokens_offset: usize,
}

pub fn dir_in(root: &Path) -> PathBuf {
    root.join(CACHE_DIR)
}

/// User-level cache root: `~/.cache/dup-detector` on Linux (or
/// `$XDG_CACHE_HOME/dup-detector`), `~/Library/Caches/dup-detector` on macOS and
/// `%LOCALAPPDATA%\dup-detector` on Windows.
pub fn user_cache_root() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join(CONFIG_DIR_NAME))
}

/// Cache directory for one scanned root under the user cache root, keyed by the
/// canonical root path so several projects sharing the directory stay separate.
pub fn user_cache_dir(root: &Path) -> Option<PathBuf> {
    Some(user_cache_dir_in(&user_cache_root()?, root))
}

fn user_cache_dir_in(base: &Path, root: &Path) -> PathBuf {
    let key = xxh3_64(root.as_os_str().as_encoded_bytes());
    base.join(format!("{key:016x}"))
}

/// Cache directory for `root` according to `location`; `None` when the user
/// cache root cannot be resolved.
pub fn dir_for(root: &Path, location: CacheLocation) -> Option<PathBuf> {
    match location {
        CacheLocation::Project => Some(dir_in(root)),
        CacheLocation::UserCache => user_cache_dir(root),
    }
}

/// Cache file for `path` inside `dir`; `path` must live under `base` (the
/// cache base — the index root, or a shared workspace root for scope indexes).
pub fn entry_path(dir: &Path, base: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(base).ok()?;
    let key = xxh3_64(relative.as_os_str().as_encoded_bytes());
    Some(dir.join(format!("{key:016x}.bin")))
}

pub fn text_hash(text: &str) -> u64 {
    text_hash_bytes(text.as_bytes())
}

pub fn text_hash_bytes(bytes: &[u8]) -> u64 {
    xxh3_64(bytes)
}

pub fn load_entry(path: &Path) -> Option<CacheEntry> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::debug!(path = %path.display(), error = %error, "cannot read cache entry");
            return None;
        }
    };
    let entry = match mmap_entry(&file) {
        Some(entry) => Some(entry),
        None => match fs::read(path) {
            Ok(bytes) => decode_entry(&bytes),
            Err(error) => {
                tracing::debug!(path = %path.display(), error = %error, "cannot read cache entry");
                None
            }
        },
    };
    if entry.is_none() {
        tracing::debug!(path = %path.display(), "ignoring unusable cache entry");
    }
    entry
}

fn mmap_entry(file: &fs::File) -> Option<CacheEntry> {
    // SAFETY: cache entries are written atomically (temp file + rename) and
    // never modified in place while they can be mapped.
    let map = unsafe { Mmap::map(file) }.ok()?;
    let header = parse_header(&map)?;
    let tokens = decode_tokens(map.get(header.tokens_offset..)?, header.token_count)?;
    Some(CacheEntry {
        path: header.path,
        modified: header.modified,
        size: header.size,
        text_hash: header.text_hash,
        grammar_fingerprint: header.grammar_fingerprint,
        tokens: Slice::owned(tokens),
    })
}

/// Writes (atomically) the cache entry for `file` into `dir`, keyed by its
/// `base`-relative path.
pub fn store_entry(dir: &Path, base: &Path, file: &SourceFile) -> io::Result<()> {
    let Some(entry_path) = entry_path(dir, base, &file.path) else {
        return Ok(());
    };
    let Some(bytes) = encode_entry(base, file) else {
        return Ok(());
    };
    write_atomic(&entry_path, &bytes)
}

/// Removes the cache entry for `path` (keyed relative to `base`) if present.
pub fn remove_entry(dir: &Path, base: &Path, path: &Path) {
    let Some(entry_path) = entry_path(dir, base, path) else {
        return;
    };
    if let Err(error) = fs::remove_file(&entry_path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(path = %entry_path.display(), error = %error, "cannot remove cache entry");
    }
}

pub fn clear(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn encode_entry(base: &Path, file: &SourceFile) -> Option<Vec<u8>> {
    let modified = file.modified?;
    let path = file.path.strip_prefix(base).ok()?.to_str()?;
    let (secs, nanos) = mtime_parts(modified)?;
    let path_bytes = path.as_bytes();
    let token_count = u32::try_from(file.tokens.len()).ok()?;
    let tokens_offset = HEADER_BYTES.checked_add(path_bytes.len())?;
    let mut out = Vec::with_capacity(tokens_offset + file.tokens.len() * 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&u32::try_from(path_bytes.len()).ok()?.to_le_bytes());
    out.extend_from_slice(&token_count.to_le_bytes());
    out.extend_from_slice(&secs.to_le_bytes());
    out.extend_from_slice(&nanos.to_le_bytes());
    out.extend_from_slice(&file.size.to_le_bytes());
    out.extend_from_slice(&text_hash_bytes(file.text.as_bytes()).to_le_bytes());
    out.extend_from_slice(
        &file
            .language
            .grammar_fingerprint()
            .unwrap_or(0)
            .to_le_bytes(),
    );
    out.extend_from_slice(path_bytes);
    encode_tokens(&mut out, file.tokens.as_slice())?;
    Some(out)
}

fn encode_tokens(out: &mut Vec<u8>, tokens: &[Token]) -> Option<()> {
    let mut previous_end = 0u32;
    for (index, token) in tokens.iter().enumerate() {
        let index = u32::try_from(index).ok()?;
        write_varint(out, token.start.checked_sub(previous_end)?);
        write_varint(out, token.end.checked_sub(token.start)?);
        out.push(token.kind | (token.flags << 2));
        if token.flags & FLAG_UNIT_START != 0 {
            write_varint(out, token.unit_end_of_start.checked_sub(index)?);
        }
        if token.flags & FLAG_UNIT_END != 0 {
            write_varint(out, index.checked_sub(token.unit_start_of_end)?);
        }
        if token.flags & FLAG_CONTAINER_START != 0 {
            write_varint(out, token.container_end_of_start.checked_sub(index)?);
        }
        previous_end = token.end;
    }
    Some(())
}

fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn parse_header(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < HEADER_BYTES || &bytes[0..8] != MAGIC {
        return None;
    }
    let mut cursor = 8;
    let path_len = read_u32(bytes, &mut cursor)? as usize;
    let token_count = read_u32(bytes, &mut cursor)? as usize;
    let secs = read_u64(bytes, &mut cursor)?;
    let nanos = read_u32(bytes, &mut cursor)?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    let size = read_u64(bytes, &mut cursor)?;
    let text_hash = read_u64(bytes, &mut cursor)?;
    let grammar_fingerprint = read_u64(bytes, &mut cursor)?;
    let path_end = HEADER_BYTES.checked_add(path_len)?;
    let path = PathBuf::from(std::str::from_utf8(bytes.get(HEADER_BYTES..path_end)?).ok()?);
    Some(Header {
        path,
        modified: UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?,
        size,
        text_hash,
        grammar_fingerprint,
        token_count,
        tokens_offset: path_end,
    })
}

fn decode_entry(bytes: &[u8]) -> Option<CacheEntry> {
    let header = parse_header(bytes)?;
    let tokens = decode_tokens(bytes.get(header.tokens_offset..)?, header.token_count)?;
    Some(CacheEntry {
        path: header.path,
        modified: header.modified,
        size: header.size,
        text_hash: header.text_hash,
        grammar_fingerprint: header.grammar_fingerprint,
        tokens: Slice::owned(tokens),
    })
}

fn decode_tokens(bytes: &[u8], count: usize) -> Option<Vec<Token>> {
    if count > bytes.len() / 3 {
        return None;
    }
    let mut tokens = Vec::with_capacity(count);
    let mut cursor = 0;
    let mut previous_end = 0u32;
    for index in 0..count {
        let index = u32::try_from(index).ok()?;
        let start = previous_end.checked_add(read_varint(bytes, &mut cursor)?)?;
        let end = start.checked_add(read_varint(bytes, &mut cursor)?)?;
        let packed = *bytes.get(cursor)?;
        cursor += 1;
        let kind = packed & 0b11;
        TokenKind::from_u8(kind)?;
        let flags = packed >> 2;
        let mut token = Token {
            start,
            end,
            unit_end_of_start: 0,
            unit_start_of_end: u32::MAX,
            container_end_of_start: 0,
            kind,
            flags,
            _pad: [0; 2],
        };
        if flags & FLAG_UNIT_START != 0 {
            token.unit_end_of_start = index.checked_add(read_varint(bytes, &mut cursor)?)?;
        }
        if flags & FLAG_UNIT_END != 0 {
            token.unit_start_of_end = index.checked_sub(read_varint(bytes, &mut cursor)?)?;
        }
        if flags & FLAG_CONTAINER_START != 0 {
            token.container_end_of_start = index.checked_add(read_varint(bytes, &mut cursor)?)?;
        }
        tokens.push(token);
        previous_end = end;
    }
    if cursor != bytes.len() {
        return None;
    }
    Some(tokens)
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    for shift in [0, 7, 14, 21, 28] {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        let part = (byte & 0x7F) as u32;
        if shift == 28 && part > 0x0F {
            return None;
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn mtime_parts(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
}

fn read_u32(raw: &[u8], cursor: &mut usize) -> Option<u32> {
    let end = cursor.checked_add(4)?;
    let value = u32::from_le_bytes(raw.get(*cursor..end)?.try_into().ok()?);
    *cursor = end;
    Some(value)
}

fn read_u64(raw: &[u8], cursor: &mut usize) -> Option<u64> {
    let end = cursor.checked_add(8)?;
    let value = u64::from_le_bytes(raw.get(*cursor..end)?.try_into().ok()?);
    *cursor = end;
    Some(value)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Unique per process and per call: concurrent stores of the same entry
    // within one process must not share a temp file.
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            match fs::remove_file(path).and_then(|()| fs::rename(&tmp, path)) {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = fs::remove_file(&tmp);
                    Err(error)
                }
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::LanguageId;
    use crate::model::{FLAG_UNIT_END, FLAG_UNIT_START};

    fn source(path: &Path, text: &str) -> SourceFile {
        SourceFile::new(
            path.to_path_buf(),
            LanguageId::RUST,
            text.to_string(),
            vec![Token {
                start: 0,
                end: 3,
                unit_end_of_start: 1,
                unit_start_of_end: 0,
                container_end_of_start: 0,
                kind: TokenKind::Identifier.as_u8(),
                flags: FLAG_UNIT_START | FLAG_UNIT_END,
                _pad: [0; 2],
            }],
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            text.len() as u64,
        )
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-cache-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn round_trips_entry() {
        let dir = temp_dir("roundtrip");
        let root = PathBuf::from("root");
        let relative = Path::new("src").join("a.rs");
        let file = source(&root.join(&relative), "let a = 1;\n");
        store_entry(&dir, &root, &file).unwrap();
        let entry = entry_path(&dir, &root, &file.path).expect("entry path");
        assert_eq!(entry.parent(), Some(dir.as_path()));
        let loaded = load_entry(&entry).expect("entry round-trips");
        assert_eq!(loaded.path, relative);
        assert_eq!(loaded.size, file.size);
        assert_eq!(loaded.text_hash, text_hash(file.text.as_str()));
        assert_eq!(
            loaded.grammar_fingerprint,
            file.language
                .grammar_fingerprint()
                .expect("rust fingerprint")
        );
        assert_eq!(loaded.tokens.len(), 1);
        assert_eq!(loaded.tokens[0].kind, TokenKind::Identifier.as_u8());
        assert!(loaded.tokens[0].unit_start());
        assert!(!loaded.tokens.is_mapped());
        clear(&dir).unwrap();
    }

    #[test]
    fn entries_use_compact_token_encoding() {
        let dir = temp_dir("tokens-only");
        let root = PathBuf::from("root");
        let relative = Path::new("src").join("a.rs");
        let file = source(&root.join(&relative), "let a = 1;\n");
        store_entry(&dir, &root, &file).unwrap();
        let entry = entry_path(&dir, &root, &file.path).expect("entry path");
        let bytes = fs::read(entry).expect("entry file");
        let mut payload = Vec::new();
        encode_tokens(&mut payload, file.tokens.as_slice()).unwrap();
        let expected = HEADER_BYTES + relative.to_str().unwrap().len() + payload.len();
        assert_eq!(bytes.len(), expected);
        assert!(payload.len() < file.tokens.len() * size_of::<Token>());
        clear(&dir).unwrap();
    }

    #[test]
    fn compact_tokens_round_trip() {
        let dir = temp_dir("compact");
        let root = PathBuf::from("root");
        let relative = Path::new("src").join("a.rs");
        let text = "fn f() { let alpha = beta + 1; }\n";
        let tokens = crate::tokenize::tokenize(text, LanguageId::RUST).unwrap();
        let file = SourceFile::new(
            root.join(&relative),
            LanguageId::RUST,
            text.to_string(),
            tokens,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            text.len() as u64,
        );
        store_entry(&dir, &root, &file).unwrap();
        let entry = entry_path(&dir, &root, &file.path).expect("entry path");
        let loaded = load_entry(&entry).expect("entry round-trips");
        assert_eq!(loaded.tokens.as_slice(), file.tokens.as_slice());
        clear(&dir).unwrap();
    }

    #[test]
    fn varints_round_trip() {
        for value in [0, 1, 127, 128, 16_383, 16_384, u32::MAX] {
            let mut bytes = Vec::new();
            write_varint(&mut bytes, value);
            let mut cursor = 0;
            assert_eq!(read_varint(&bytes, &mut cursor), Some(value));
            assert_eq!(cursor, bytes.len());
        }
    }

    #[test]
    fn rejects_truncated_varint() {
        let mut cursor = 0;
        assert_eq!(read_varint(&[0x80], &mut cursor), None);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&[0xFF, 0xFF, 0xFF, 0xFF, 0x1F], &mut cursor),
            None
        );
    }

    #[test]
    fn dir_for_selects_location() {
        let root = Path::new("/tmp/project");
        assert_eq!(
            dir_for(root, CacheLocation::Project),
            Some(root.join(CACHE_DIR))
        );
        assert_eq!(
            dir_for(root, CacheLocation::UserCache),
            user_cache_root().map(|base| user_cache_dir_in(&base, root))
        );
    }

    #[test]
    fn user_cache_dirs_are_keyed_by_root() {
        let base = PathBuf::from("/base");
        let a = user_cache_dir_in(&base, Path::new("/p/a"));
        let b = user_cache_dir_in(&base, Path::new("/p/b"));
        assert_eq!(a, user_cache_dir_in(&base, Path::new("/p/a")));
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(base.as_path()));
        let name = a.file_name().and_then(|name| name.to_str()).unwrap();
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn entry_names_are_relative_to_root() {
        let dir = temp_dir("portable");
        let root_a = PathBuf::from("a");
        let root_b = PathBuf::from("b");
        let relative = Path::new("src").join("x.rs");
        let a = entry_path(&dir, &root_a, &root_a.join(&relative));
        let b = entry_path(&dir, &root_b, &root_b.join(&relative));
        assert_eq!(a, b);
        let c = entry_path(&dir, &root_a, &root_a.join("src").join("y.rs"));
        assert_ne!(a, c);
    }

    #[test]
    fn rejects_corrupt_entry() {
        let dir = temp_dir("corrupt");
        let path = dir.join("entry.bin");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, b"not a cache").unwrap();
        assert!(load_entry(&path).is_none());
        fs::write(&path, b"DUPCDT05\x01\x00\x00\x00garbage").unwrap();
        assert!(load_entry(&path).is_none());
        clear(&dir).unwrap();
    }

    #[test]
    fn missing_entry_is_none() {
        let path = temp_dir("missing").join("entry.bin");
        assert!(load_entry(&path).is_none());
    }

    #[test]
    fn clear_removes_directory() {
        let dir = temp_dir("clear");
        let root = PathBuf::from("root");
        let file = source(&root.join("a.rs"), "let a = 1;\n");
        store_entry(&dir, &root, &file).unwrap();
        assert!(dir.exists());
        clear(&dir).unwrap();
        assert!(!dir.exists());
        clear(&dir).unwrap();
    }

    #[test]
    fn entries_are_distinct_per_path() {
        let dir = temp_dir("distinct");
        let root = PathBuf::from("root");
        let a = entry_path(&dir, &root, &root.join("a.rs")).expect("entry path");
        let b = entry_path(&dir, &root, &root.join("b.rs")).expect("entry path");
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(dir.as_path()));
    }
}
