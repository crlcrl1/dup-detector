use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;
use xxhash_rust::xxh3::xxh3_64;

use crate::config::{CONFIG_DIR_NAME, CacheLocation};
use crate::model::{Slice, SourceFile, Token, TokenKind};

const MAGIC: &[u8; 8] = b"DUPCDT10";
const HEADER_BYTES: usize = 44;

pub const CACHE_DIR: &str = ".dup-detector";

pub struct CacheEntry {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: u64,
    pub text_hash: u64,
    pub tokens: Slice<Token>,
    pub hashes: Slice<u64>,
}

struct Header {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    text_hash: u64,
    token_count: usize,
    hashes_offset: usize,
    tokens_offset: usize,
}

fn align8(value: usize) -> usize {
    (value + 7) & !7
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

pub fn entry_path(dir: &Path, root: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(root).ok()?;
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

#[cfg(target_endian = "little")]
fn mmap_entry(file: &fs::File) -> Option<CacheEntry> {
    // SAFETY: cache entries are written atomically (temp file + rename) and
    // never modified in place while they can be mapped.
    let map = Arc::new(unsafe { Mmap::map(file) }.ok()?);
    let header = parse_header(&map)?;
    let (tokens, hashes) = if header.token_count == 0 {
        (Slice::owned(Vec::new()), Slice::owned(Vec::new()))
    } else {
        let hashes = Slice::mapped(map.clone(), header.hashes_offset, header.token_count)?;
        let tokens = Slice::mapped(map, header.tokens_offset, header.token_count)?;
        (tokens, hashes)
    };
    Some(CacheEntry {
        path: header.path,
        modified: header.modified,
        size: header.size,
        text_hash: header.text_hash,
        tokens,
        hashes,
    })
}

#[cfg(not(target_endian = "little"))]
fn mmap_entry(_file: &fs::File) -> Option<CacheEntry> {
    None
}

pub fn store_entry(dir: &Path, root: &Path, file: &SourceFile) -> io::Result<()> {
    let Some(entry_path) = entry_path(dir, root, &file.path) else {
        return Ok(());
    };
    let Some(bytes) = encode_entry(root, file) else {
        return Ok(());
    };
    write_atomic(&entry_path, &bytes)
}

pub fn remove_entry(dir: &Path, root: &Path, path: &Path) {
    let Some(entry_path) = entry_path(dir, root, path) else {
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

fn encode_entry(root: &Path, file: &SourceFile) -> Option<Vec<u8>> {
    let modified = file.modified?;
    let path = file.path.strip_prefix(root).ok()?.to_str()?;
    let (secs, nanos) = mtime_parts(modified)?;
    let path_bytes = path.as_bytes();
    let token_count = u32::try_from(file.tokens.len()).ok()?;
    if file.hashes.len() != file.tokens.len() {
        return None;
    }
    let hashes_offset = align8(HEADER_BYTES + path_bytes.len());
    let tokens_offset = hashes_offset + file.hashes.len() * size_of::<u64>();
    let total = tokens_offset + file.tokens.len() * size_of::<Token>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&u32::try_from(path_bytes.len()).ok()?.to_le_bytes());
    out.extend_from_slice(&token_count.to_le_bytes());
    out.extend_from_slice(&secs.to_le_bytes());
    out.extend_from_slice(&nanos.to_le_bytes());
    out.extend_from_slice(&file.size.to_le_bytes());
    out.extend_from_slice(&text_hash_bytes(file.text.as_bytes()).to_le_bytes());
    out.extend_from_slice(path_bytes);
    out.resize(hashes_offset, 0);
    for hash in file.hashes.as_slice() {
        out.extend_from_slice(&hash.to_le_bytes());
    }
    append_tokens(&mut out, file.tokens.as_slice());
    Some(out)
}

#[cfg(target_endian = "little")]
fn append_tokens(out: &mut Vec<u8>, tokens: &[Token]) {
    out.extend_from_slice(bytemuck::cast_slice(tokens));
}

#[cfg(not(target_endian = "little"))]
fn append_tokens(out: &mut Vec<u8>, tokens: &[Token]) {
    for token in tokens {
        out.extend_from_slice(&token.start.to_le_bytes());
        out.extend_from_slice(&token.end.to_le_bytes());
        out.extend_from_slice(&token.unit_end_of_start.to_le_bytes());
        out.extend_from_slice(&token.unit_start_of_end.to_le_bytes());
        out.extend_from_slice(&token.container_end_of_start.to_le_bytes());
        out.push(token.kind);
        out.push(token.flags);
        out.extend_from_slice(&token._pad);
    }
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
    let path_end = HEADER_BYTES.checked_add(path_len)?;
    let path = PathBuf::from(std::str::from_utf8(bytes.get(HEADER_BYTES..path_end)?).ok()?);
    let hashes_offset = align8(path_end);
    let tokens_offset = hashes_offset.checked_add(token_count.checked_mul(size_of::<u64>())?)?;
    let end = tokens_offset.checked_add(token_count.checked_mul(size_of::<Token>())?)?;
    if end > bytes.len() || tokens_offset % align_of::<Token>() != 0 {
        return None;
    }
    Some(Header {
        path,
        modified: UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?,
        size,
        text_hash,
        token_count,
        hashes_offset,
        tokens_offset,
    })
}

fn decode_entry(bytes: &[u8]) -> Option<CacheEntry> {
    let header = parse_header(bytes)?;
    let count = header.token_count;
    let hash_bytes = bytes.get(header.hashes_offset..header.hashes_offset + count * 8)?;
    let hashes = hash_bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    let raw = bytes.get(header.tokens_offset..header.tokens_offset + count * size_of::<Token>())?;
    let mut tokens = Vec::with_capacity(count);
    for chunk in raw.as_chunks::<{ size_of::<Token>() }>().0 {
        tokens.push(decode_token(chunk)?);
    }
    Some(CacheEntry {
        path: header.path,
        modified: header.modified,
        size: header.size,
        text_hash: header.text_hash,
        tokens: Slice::owned(tokens),
        hashes: Slice::owned(hashes),
    })
}

fn decode_token(raw: &[u8]) -> Option<Token> {
    let kind = TokenKind::from_u8(*raw.get(20)?)?;
    Some(Token {
        start: read_u32_from(raw, 0)?,
        end: read_u32_from(raw, 4)?,
        unit_end_of_start: read_u32_from(raw, 8)?,
        unit_start_of_end: read_u32_from(raw, 12)?,
        container_end_of_start: read_u32_from(raw, 16)?,
        kind: kind.as_u8(),
        flags: *raw.get(21)?,
        _pad: [0; 2],
    })
}

fn mtime_parts(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
}

fn read_u32_from(raw: &[u8], offset: usize) -> Option<u32> {
    let mut cursor = offset;
    read_u32(raw, &mut cursor)
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
            LanguageId::Rust,
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
        assert_eq!(loaded.tokens.len(), 1);
        assert_eq!(loaded.tokens[0].kind, TokenKind::Identifier.as_u8());
        assert!(loaded.tokens[0].unit_start());
        assert_eq!(loaded.hashes.as_slice(), file.hashes.as_slice());
        #[cfg(target_endian = "little")]
        {
            assert!(loaded.tokens.is_mapped());
            assert!(loaded.hashes.is_mapped());
        }
        clear(&dir).unwrap();
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
