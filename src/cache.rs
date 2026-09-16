use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xxhash_rust::xxh3::xxh3_64;

use crate::model::{SourceFile, Token, TokenKind};

const MAGIC: &[u8; 8] = b"DUPCDT04";
const HEADER_BYTES: usize = 12;
const TOKEN_BYTES: usize = 34;

pub const CACHE_DIR: &str = ".dup-detector";

pub struct CacheEntry {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: u64,
    pub text_hash: u64,
    pub tokens: Vec<Token>,
    pub hashes: Vec<u64>,
}

pub fn dir_in(root: &Path) -> PathBuf {
    root.join(CACHE_DIR)
}

pub fn entry_path(dir: &Path, root: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(root).ok()?;
    let key = xxh3_64(relative.as_os_str().as_encoded_bytes());
    Some(dir.join(format!("{key:016x}.bin")))
}

pub fn text_hash(text: &str) -> u64 {
    xxh3_64(text.as_bytes())
}

pub fn load_entry(path: &Path) -> Option<CacheEntry> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::debug!(path = %path.display(), error = %error, "cannot read cache entry");
            return None;
        }
    };
    let entry = decode_entry(&bytes);
    if entry.is_none() {
        tracing::debug!(path = %path.display(), "ignoring unusable cache entry");
    }
    entry
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
    let mut out = Vec::with_capacity(
        HEADER_BYTES
            + path_bytes.len()
            + 28
            + file.tokens.len() * (TOKEN_BYTES + std::mem::size_of::<u64>()),
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&u32::try_from(path_bytes.len()).ok()?.to_le_bytes());
    out.extend_from_slice(path_bytes);
    out.extend_from_slice(&secs.to_le_bytes());
    out.extend_from_slice(&nanos.to_le_bytes());
    out.extend_from_slice(&file.size.to_le_bytes());
    out.extend_from_slice(&text_hash(&file.text).to_le_bytes());
    out.extend_from_slice(&token_count.to_le_bytes());
    encode_tokens(&mut out, &file.tokens);
    for hash in &file.hashes {
        out.extend_from_slice(&hash.to_le_bytes());
    }
    Some(out)
}

fn decode_entry(bytes: &[u8]) -> Option<CacheEntry> {
    if bytes.len() < HEADER_BYTES || &bytes[0..8] != MAGIC {
        return None;
    }
    let mut cursor = 8;
    let path_len = read_u32(bytes, &mut cursor)? as usize;
    let path_end = cursor.checked_add(path_len)?;
    let path = PathBuf::from(std::str::from_utf8(bytes.get(cursor..path_end)?).ok()?);
    cursor = path_end;
    let secs = read_u64(bytes, &mut cursor)?;
    let nanos = read_u32(bytes, &mut cursor)?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    let size = read_u64(bytes, &mut cursor)?;
    let text_hash = read_u64(bytes, &mut cursor)?;
    let token_count = read_u32(bytes, &mut cursor)? as usize;
    let tokens_end = cursor.checked_add(token_count.checked_mul(TOKEN_BYTES)?)?;
    let tokens = decode_tokens(bytes.get(cursor..tokens_end)?, token_count)?;
    let hashes_end = tokens_end.checked_add(token_count.checked_mul(8)?)?;
    let hashes = bytes
        .get(tokens_end..hashes_end)?
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    Some(CacheEntry {
        path,
        modified: UNIX_EPOCH + Duration::new(secs, nanos),
        size,
        text_hash,
        tokens,
        hashes,
    })
}

fn mtime_parts(time: SystemTime) -> Option<(u64, u32)> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
}

fn encode_tokens(out: &mut Vec<u8>, tokens: &[Token]) {
    for token in tokens {
        let mut flags = 0u8;
        if token.unit_start {
            flags |= 1;
        }
        if token.unit_end {
            flags |= 2;
        }
        if token.container_start {
            flags |= 4;
        }
        out.push(match token.kind {
            TokenKind::Identifier => 0,
            TokenKind::Literal => 1,
            TokenKind::Fixed => 2,
        });
        out.push(flags);
        out.extend_from_slice(&token.start.to_le_bytes());
        out.extend_from_slice(&token.end.to_le_bytes());
        out.extend_from_slice(&token.line.to_le_bytes());
        out.extend_from_slice(&token.end_line.to_le_bytes());
        out.extend_from_slice(&token.column.to_le_bytes());
        out.extend_from_slice(&token.unit_end_of_start.to_le_bytes());
        out.extend_from_slice(&token.unit_start_of_end.to_le_bytes());
        out.extend_from_slice(&token.container_end_of_start.to_le_bytes());
    }
}

fn decode_tokens(raw: &[u8], count: usize) -> Option<Vec<Token>> {
    if raw.len() != count.checked_mul(TOKEN_BYTES)? {
        return None;
    }
    let mut tokens = Vec::with_capacity(count);
    for chunk in raw.as_chunks::<TOKEN_BYTES>().0 {
        let kind = match chunk[0] {
            0 => TokenKind::Identifier,
            1 => TokenKind::Literal,
            2 => TokenKind::Fixed,
            _ => return None,
        };
        let flags = chunk[1];
        tokens.push(Token {
            kind,
            start: read_u32_from(chunk, 2)?,
            end: read_u32_from(chunk, 6)?,
            line: read_u32_from(chunk, 10)?,
            end_line: read_u32_from(chunk, 14)?,
            column: read_u32_from(chunk, 18)?,
            unit_start: flags & 1 != 0,
            unit_end: flags & 2 != 0,
            unit_end_of_start: read_u32_from(chunk, 22)?,
            unit_start_of_end: read_u32_from(chunk, 26)?,
            container_start: flags & 4 != 0,
            container_end_of_start: read_u32_from(chunk, 30)?,
        });
    }
    Some(tokens)
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
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            fs::remove_file(path)?;
            fs::rename(&tmp, path)
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

    fn source(path: &Path, text: &str) -> SourceFile {
        SourceFile::new(
            path.to_path_buf(),
            LanguageId::Rust,
            text.to_string(),
            vec![Token {
                kind: TokenKind::Identifier,
                start: 0,
                end: 3,
                line: 1,
                end_line: 1,
                column: 0,
                unit_start: true,
                unit_end: true,
                unit_end_of_start: 1,
                unit_start_of_end: 0,
                container_start: false,
                container_end_of_start: 0,
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
        assert_eq!(loaded.text_hash, text_hash(&file.text));
        assert_eq!(loaded.tokens.len(), 1);
        assert_eq!(loaded.tokens[0].kind, TokenKind::Identifier);
        assert!(loaded.tokens[0].unit_start);
        assert_eq!(loaded.hashes, file.hashes);
        clear(&dir).unwrap();
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
        fs::write(&path, b"DUPCDT04\x01\x00\x00\x00garbage").unwrap();
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
