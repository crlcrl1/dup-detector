use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use thiserror::Error;

use crate::cache;
use crate::config::Config;
use crate::detect;
use crate::language::LanguageId;
use crate::model::{CloneGroup, SourceFile};
use crate::tokenize;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to walk {root}: {source}")]
    Walk {
        root: PathBuf,
        source: ignore::Error,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildStats {
    pub cache_hits: usize,
    pub parsed_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    /// Stable: the file exceeds `max_file_bytes`, so retrying is pointless.
    Oversized,
    /// Transient: stat/read/tokenize failed; retried on every refresh so a
    /// permission fix or a transient I/O error is picked up without reindex.
    Unreadable,
}

/// Metadata recorded when a file was skipped, used to keep the refresh fast
/// path valid and to avoid re-attempting unchanged oversized files.
type SkipRecord = (SkipReason, Option<SystemTime>, u64);

fn current_skip_record(path: &Path, max_file_bytes: u64) -> SkipRecord {
    match fs::metadata(path) {
        Ok(meta) => {
            let reason = if meta.len() > max_file_bytes {
                SkipReason::Oversized
            } else {
                SkipReason::Unreadable
            };
            (reason, meta.modified().ok(), meta.len())
        }
        Err(_) => (SkipReason::Unreadable, None, 0),
    }
}

pub struct SourceIndex {
    root: PathBuf,
    config: Config,
    files: Vec<SourceFile>,
    by_path: HashMap<PathBuf, usize>,
    skipped: HashMap<PathBuf, SkipRecord>,
    cache_dir: Option<PathBuf>,
    /// Cache entry keys (and the relative paths stored in entries) are computed
    /// against this directory instead of `root`, so several indexes covering
    /// different scopes of one workspace share a single on-disk entry per file.
    cache_base: PathBuf,
    stats: BuildStats,
}

impl SourceIndex {
    pub fn build(root: impl AsRef<Path>, config: &Config) -> Result<Self, IndexError> {
        let root = canonicalize_root(root)?;
        let cache_dir = cache::dir_for(&root, config.cache_location);
        Self::build_at(root, config, cache_dir, None)
    }

    pub fn build_with_cache(
        root: impl AsRef<Path>,
        config: &Config,
        cache_dir: Option<&Path>,
    ) -> Result<Self, IndexError> {
        let root = canonicalize_root(root)?;
        Self::build_at(root, config, cache_dir.map(Path::to_path_buf), None)
    }

    /// Like [`SourceIndex::build_with_cache`], but cache entries are keyed by
    /// `cache_base`-relative paths, letting one cache directory serve several
    /// scope indexes of the same workspace without duplicating entries.
    pub fn build_with_cache_base(
        root: impl AsRef<Path>,
        config: &Config,
        cache_dir: Option<&Path>,
        cache_base: impl AsRef<Path>,
    ) -> Result<Self, IndexError> {
        let root = canonicalize_root(root)?;
        let cache_base = cache_base.as_ref().to_path_buf();
        Self::build_at(
            root,
            config,
            cache_dir.map(Path::to_path_buf),
            Some(cache_base),
        )
    }

    fn build_at(
        root: PathBuf,
        config: &Config,
        cache_dir: Option<PathBuf>,
        cache_base: Option<PathBuf>,
    ) -> Result<Self, IndexError> {
        let started = std::time::Instant::now();
        let max_file_bytes = config.max_file_bytes;
        let cache_base = cache_base.unwrap_or_else(|| root.clone());
        let found = discover(&root, config)?;
        let live: Option<HashSet<String>> =
            (cache_dir.is_some() && root == cache_base).then(|| {
                found
                    .iter()
                    .filter_map(|(path, _, _)| cache::entry_name(&cache_base, path))
                    .collect()
            });
        let paths = enabled(found, config);
        let discover_ms = started.elapsed().as_millis();
        let mut slots: Vec<Option<SourceFile>> = paths
            .par_iter()
            .map(|(path, language, meta)| {
                let dir = cache_dir.as_deref()?;
                file_from_cache(
                    dir,
                    &cache_base,
                    path,
                    *language,
                    max_file_bytes,
                    meta.as_ref()?,
                )
            })
            .collect();
        let load_ms = started.elapsed().as_millis() - discover_ms;
        let mut misses: Vec<usize> = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            if slot.is_none() {
                misses.push(index);
            }
        }
        let cache_hits = paths.len() - misses.len();
        let parsed_files = misses.len();
        let parsed: Vec<Option<SourceFile>> = misses
            .par_iter()
            .map(|&index| {
                let (path, language) = (&paths[index].0, paths[index].1);
                let parsed = parse_file(path, language, max_file_bytes)?;
                if let Some(dir) = &cache_dir
                    && let Err(error) = cache::store_entry(dir, &cache_base, &parsed)
                {
                    tracing::debug!(error = %error, "cannot write cache entry");
                }
                Some(parsed)
            })
            .collect();
        for (slot, file) in misses.into_iter().zip(parsed) {
            slots[slot] = file;
        }
        if parsed_files > 0
            && let Some(dir) = &cache_dir
        {
            if let Some(live) = &live {
                cache::retain_entries(dir, live);
            }
            if let Some(base) = cache::user_cache_root()
                && dir.parent() == Some(base.as_path())
            {
                cache::mark_root(dir, &cache_base);
                cache::collect_stale_roots(&base);
            }
        }
        let mut skipped: HashMap<PathBuf, SkipRecord> = HashMap::new();
        for (index, slot) in slots.iter().enumerate() {
            if slot.is_none() {
                skipped.insert(
                    paths[index].0.clone(),
                    current_skip_record(&paths[index].0, max_file_bytes),
                );
            }
        }
        let files: Vec<SourceFile> = slots.into_iter().flatten().collect();
        let stats = BuildStats {
            cache_hits,
            parsed_files,
        };
        let index = Self::from_parts(
            root,
            config.clone(),
            files,
            skipped,
            cache_dir,
            cache_base,
            stats,
        );
        tracing::debug!(
            discover_ms,
            load_ms,
            total_ms = started.elapsed().as_millis(),
            cache_hits = index.stats.cache_hits,
            parsed_files = index.stats.parsed_files,
            files = index.files.len(),
            "index built"
        );
        Ok(index)
    }

    /// Removes the on-disk cache for `root` in both possible locations and
    /// returns the directories that existed and were cleared.
    pub fn clear_cache(root: &Path) -> Vec<PathBuf> {
        let Ok(root) = dunce::canonicalize(root) else {
            return Vec::new();
        };
        let mut dirs = vec![cache::dir_in(&root)];
        if let Some(dir) = cache::user_cache_dir(&root) {
            dirs.push(dir);
        }
        let mut cleared = Vec::new();
        for dir in dirs {
            if !dir.exists() {
                continue;
            }
            match cache::clear(&dir) {
                Ok(()) => cleared.push(dir),
                Err(error) => tracing::debug!(
                    root = %root.display(),
                    dir = %dir.display(),
                    error = %error,
                    "cannot clear cache"
                ),
            }
        }
        cleared
    }

    pub fn refresh(&mut self) -> Vec<PathBuf> {
        let paths = match discover(&self.root, &self.config) {
            Ok(found) => enabled(found, &self.config),
            Err(e) => {
                tracing::warn!(error = %e, "refresh: file discovery failed");
                return Vec::new();
            }
        };
        let max_file_bytes = self.config.max_file_bytes;
        let stamps: Vec<Option<fs::Metadata>> = paths
            .par_iter()
            .map(|(path, _, _)| fs::metadata(path).ok())
            .collect();
        let unchanged = |file: &SourceFile, meta: &Option<fs::Metadata>| {
            meta.as_ref().is_some_and(|meta| {
                file.modified == meta.modified().ok() && file.size == meta.len()
            })
        };
        if paths.len() == self.files.len() + self.skipped.len()
            && paths.par_iter().zip(&stamps).all(|((path, _, _), meta)| {
                if let Some(&slot) = self.by_path.get(path) {
                    unchanged(&self.files[slot], meta)
                } else if let Some(record) = self.skipped.get(path) {
                    // Only oversized files are stable enough to skip forever;
                    // everything else takes the slow path so it is retried.
                    record.0 == SkipReason::Oversized
                        && *record == current_skip_record(path, max_file_bytes)
                } else {
                    false
                }
            })
        {
            return Vec::new();
        }
        let old: HashMap<PathBuf, SourceFile> = std::mem::take(&mut self.files)
            .into_iter()
            .map(|f| (f.path.clone(), f))
            .collect();
        let reusable: Vec<bool> = paths
            .par_iter()
            .zip(&stamps)
            .map(|((path, _, _), meta)| old.get(path).is_some_and(|f| unchanged(f, meta)))
            .collect();
        let mut old = old;
        let mut slots: Vec<Option<SourceFile>> = Vec::with_capacity(paths.len());
        let mut changed = Vec::new();
        let mut misses: Vec<usize> = Vec::new();
        let mut skipped: HashMap<PathBuf, SkipRecord> = HashMap::new();
        for (index, (path, _, _)) in paths.iter().enumerate() {
            let existing = old.remove(path);
            if reusable[index] {
                slots.push(existing);
                continue;
            }
            // An oversized file whose metadata is unchanged is not
            // re-attempted; unreadable files always fall through so they are
            // retried.
            if let Some(record) = self.skipped.get(path)
                && record.0 == SkipReason::Oversized
                && *record == current_skip_record(path, max_file_bytes)
            {
                skipped.insert(path.clone(), *record);
                slots.push(None);
                continue;
            }
            misses.push(index);
            changed.push(path.clone());
            slots.push(None);
        }
        let root = &self.cache_base;
        let cache_dir = &self.cache_dir;
        let parsed: Vec<(Option<SourceFile>, bool)> = misses
            .par_iter()
            .map(|&index| {
                let (path, language) = (&paths[index].0, paths[index].1);
                // The disk entry still covers files that changed since the last
                // refresh but not since their entry was written.
                if let Some(dir) = cache_dir
                    && let Some(meta) = &stamps[index]
                    && let Some(file) =
                        file_from_cache(dir, root, path, language, max_file_bytes, meta)
                {
                    return (Some(file), true);
                }
                let Some(parsed) = parse_file(path, language, max_file_bytes) else {
                    return (None, false);
                };
                if let Some(dir) = cache_dir
                    && let Err(error) = cache::store_entry(dir, root, &parsed)
                {
                    tracing::debug!(error = %error, "cannot write cache entry");
                }
                (Some(parsed), false)
            })
            .collect();
        let cache_hits = parsed.iter().filter(|(_, hit)| *hit).count();
        let parsed_count = parsed
            .iter()
            .filter(|(file, hit)| file.is_some() && !*hit)
            .count();
        let mut failed = Vec::new();
        for (slot, (file, _)) in misses.into_iter().zip(parsed) {
            if file.is_none() {
                failed.push(paths[slot].0.clone());
            }
            slots[slot] = file;
        }
        if let Some(dir) = &self.cache_dir {
            for removed in old.values() {
                cache::remove_entry(dir, &self.cache_base, &removed.path);
            }
            for path in &failed {
                cache::remove_entry(dir, &self.cache_base, path);
            }
        }
        for path in &failed {
            skipped.insert(path.clone(), current_skip_record(path, max_file_bytes));
        }
        changed.extend(old.into_keys());
        self.files = slots.into_iter().flatten().collect();
        self.by_path = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        self.skipped = skipped;
        self.stats = BuildStats {
            cache_hits,
            parsed_files: parsed_count,
        };
        changed
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn file_by_path(&self, path: &Path) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub fn stats(&self) -> BuildStats {
        self.stats
    }

    pub fn find_clones(&self, config: &Config) -> Vec<CloneGroup> {
        let files: Vec<&SourceFile> = self.files.iter().collect();
        detect::detect(&files, config)
    }

    pub fn files_with<'a>(
        &'a self,
        overlays: &'a [SourceFile],
        exclude: &HashSet<PathBuf>,
    ) -> (Vec<&'a SourceFile>, Vec<usize>) {
        let latest: HashMap<&Path, usize> = overlays
            .iter()
            .enumerate()
            .map(|(index, overlay)| (overlay.path.as_path(), index))
            .collect();
        let mut files: Vec<&SourceFile> = self
            .files
            .iter()
            .filter(|file| {
                !exclude.contains(&file.path) && !latest.contains_key(file.path.as_path())
            })
            .collect();
        let mut positions = Vec::new();
        for (index, overlay) in overlays.iter().enumerate() {
            if latest.get(overlay.path.as_path()) == Some(&index)
                && !exclude.contains(&overlay.path)
            {
                files.push(overlay);
                positions.push(files.len() - 1);
            }
        }
        (files, positions)
    }

    fn from_parts(
        root: PathBuf,
        config: Config,
        files: Vec<SourceFile>,
        skipped: HashMap<PathBuf, SkipRecord>,
        cache_dir: Option<PathBuf>,
        cache_base: PathBuf,
        stats: BuildStats,
    ) -> Self {
        let by_path = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        Self {
            root,
            config,
            files,
            by_path,
            skipped,
            cache_dir,
            cache_base,
            stats,
        }
    }
}

fn canonicalize_root(root: impl AsRef<Path>) -> Result<PathBuf, IndexError> {
    dunce::canonicalize(&root).map_err(|e| IndexError::Io {
        path: root.as_ref().to_path_buf(),
        source: e,
    })
}

fn file_from_cache(
    dir: &Path,
    base: &Path,
    path: &Path,
    language: LanguageId,
    max_file_bytes: u64,
    meta: &fs::Metadata,
) -> Option<SourceFile> {
    let entry = cache::load_entry(&cache::entry_path(dir, base, path)?)?;
    if entry.path != path.strip_prefix(base).ok()? {
        return None;
    }
    if entry.size > max_file_bytes {
        return None;
    }
    if meta.len() != entry.size || meta.modified().ok() != Some(entry.modified) {
        return None;
    }
    let text = match mapped_text(path, entry.size) {
        Some(text) => text,
        None => crate::model::Text::Owned(decode_source(fs::read(path).ok()?)),
    };
    if cache::text_hash_bytes(text.as_bytes()) != entry.text_hash {
        return None;
    }
    if entry.grammar_fingerprint != language.grammar_fingerprint().unwrap_or(u64::MAX) {
        return None;
    }
    if entry
        .tokens
        .iter()
        .any(|token| token.start > token.end || token.end as usize > text.len())
    {
        return None;
    }
    let hashes = crate::encode::token_hashes(text.as_str(), entry.tokens.as_slice());
    let file = SourceFile::from_storage(
        path.to_path_buf(),
        language,
        text,
        entry.tokens,
        crate::model::Slice::owned(hashes),
        Some(entry.modified),
        entry.size,
    );
    file.text.release_pages();
    Some(file)
}

#[cfg(unix)]
fn mapped_text(path: &Path, expected_size: u64) -> Option<crate::model::Text> {
    // Mapping a file that is concurrently truncated raises SIGBUS on access,
    // so only return a mapping whose metadata stayed stable across the map;
    // otherwise fall back to the owned read path, which narrows the window to
    // a truncation that lands after the final stat.
    for _ in 0..2 {
        let before = fs::metadata(path).ok()?;
        if before.len() != expected_size {
            return None;
        }
        let file = fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() != expected_size {
            continue;
        }
        // SAFETY: the mapping is read-only; the source file is replaced (not
        // truncated) by editors and build tools, so existing mappings stay
        // valid. The post-map stability check below narrows the window.
        let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        let after = fs::metadata(path).ok()?;
        if after.len() == expected_size && after.modified().ok() == before.modified().ok() {
            return crate::model::Text::mapped(std::sync::Arc::new(map));
        }
    }
    None
}

#[cfg(not(unix))]
fn mapped_text(_path: &Path, _expected_size: u64) -> Option<crate::model::Text> {
    None
}

type Discovered = (PathBuf, LanguageId, Option<fs::Metadata>);

fn discover(root: &Path, config: &Config) -> Result<Vec<Discovered>, IndexError> {
    // An inaccessible root must be an error, not an empty scan.
    if let Err(source) = fs::metadata(root) {
        return Err(IndexError::Io {
            path: root.to_path_buf(),
            source,
        });
    }
    let mut builder = WalkBuilder::new(root);
    builder
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != cache::CACHE_DIR);
    if config.no_ignore {
        builder
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false);
    }
    // Hidden entries (dot-directories like `.github`) are skipped by default,
    // matching ripgrep; `include_hidden` opts back in.
    builder.hidden(!config.include_hidden);
    let paths = Mutex::new(Vec::new());
    let failures = Mutex::new(0usize);
    builder.build_parallel().run(|| {
        Box::new(|entry| match entry {
            Ok(entry) => {
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && let Some(language) = LanguageId::from_path(entry.path())
                {
                    let metadata = config
                        .language_enabled(language)
                        .then(|| entry.metadata().ok())
                        .flatten();
                    if let Ok(mut paths) = paths.lock() {
                        paths.push((entry.into_path(), language, metadata));
                    }
                }
                WalkState::Continue
            }
            Err(error) => {
                // A single unreadable entry (permissions, deletion races during
                // a parallel build) must not abort the whole scan.
                if let Ok(mut failures) = failures.lock() {
                    *failures += 1;
                }
                tracing::debug!(error = %error, "skipping unreadable entry");
                WalkState::Continue
            }
        })
    });
    let failures = failures.into_inner().unwrap_or_default();
    if failures > 0 {
        tracing::warn!(
            failures,
            root = %root.display(),
            "skipped unreadable entries while walking"
        );
    }
    let mut paths = paths.into_inner().unwrap_or_default();
    paths.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(paths)
}

fn enabled(found: Vec<Discovered>, config: &Config) -> Vec<Discovered> {
    found
        .into_iter()
        .filter(|(_, language, _)| config.language_enabled(*language))
        .collect()
}

pub fn decode_source(bytes: Vec<u8>) -> String {
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()),
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| unit(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

pub fn parse_file(path: &Path, language: LanguageId, max_file_bytes: u64) -> Option<SourceFile> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot stat file");
            return None;
        }
    };
    if meta.len() > max_file_bytes {
        tracing::debug!(path = %path.display(), size = meta.len(), "skipping large file");
        return None;
    }
    let text = match mapped_text(path, meta.len()) {
        Some(text) => text,
        None => match fs::read(path) {
            Ok(bytes) => crate::model::Text::Owned(decode_source(bytes)),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read file");
                return None;
            }
        },
    };
    let tokens = match tokenize::tokenize(text.as_str(), language) {
        Ok(tokens) => tokens,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot tokenize");
            return None;
        }
    };
    let file = SourceFile::new_with_text(
        path.to_path_buf(),
        language,
        text,
        tokens,
        meta.modified().ok(),
        meta.len(),
    );
    file.text.release_pages();
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheLocation;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-test-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "bin"))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn discovers_supported_files_only() {
        let dir = temp_dir("discover");
        fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        fs::write(dir.join("b.py"), "def b(): pass").unwrap();
        fs::write(dir.join("notes.txt"), "not code").unwrap();
        let config = Config {
            languages: Some(vec![LanguageId::RUST]),
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 1);
        assert_eq!(index.files()[0].path.file_name().unwrap(), "a.rs");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn respects_gitignore_without_git_repo() {
        let dir = temp_dir("gitignore-no-git");
        fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
        fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        let vendor = dir.join("vendor");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("b.rs"), "fn b() {}").unwrap();
        let index = SourceIndex::build_with_cache(&dir, &Config::default(), None).unwrap();
        assert_eq!(index.files().len(), 1);
        assert_eq!(index.files()[0].path.file_name().unwrap(), "a.rs");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_files_above_max_file_bytes() {
        let dir = temp_dir("max-file-bytes");
        fs::write(dir.join("small.rs"), "fn small() {}").unwrap();
        fs::write(
            dir.join("large.rs"),
            format!("fn large() {{ let x = \"{}\"; }}", "x".repeat(4096)),
        )
        .unwrap();
        let config = Config {
            max_file_bytes: 1024,
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 1);
        assert_eq!(index.files()[0].path.file_name().unwrap(), "small.rs");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_ignore_walks_gitignored_files() {
        let dir = temp_dir("no-ignore");
        fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
        fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        let vendor = dir.join("vendor");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("b.rs"), "fn b() {}").unwrap();
        let config = Config {
            no_ignore: true,
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hidden_directories_are_skipped_unless_include_hidden() {
        let dir = temp_dir("hidden");
        fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        let hidden = dir.join(".config");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("b.rs"), "fn b() {}").unwrap();
        let index = SourceIndex::build_with_cache(&dir, &Config::default(), None).unwrap();
        assert_eq!(index.files().len(), 1);
        let config = Config {
            include_hidden: true,
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refresh_reparses_changed_files() {
        let dir = temp_dir("refresh");
        let file_path = dir.join("a.rs");
        fs::write(&file_path, "fn a() { let x = 1; }\n").unwrap();
        let mut index = SourceIndex::build_with_cache(&dir, &Config::default(), None).unwrap();
        let before = index.files()[0].tokens.len();
        fs::write(
            &file_path,
            "fn a() { let x = 1; let y = 2; let z = 3; let w = 4; }\n",
        )
        .unwrap();
        index.refresh();
        assert_eq!(index.files().len(), 1);
        assert!(index.files()[0].tokens.len() > before);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refresh_fast_path_tolerates_skipped_files() {
        let dir = temp_dir("refresh-skipped");
        fs::write(dir.join("small.rs"), "fn small() {}").unwrap();
        let large = dir.join("large.rs");
        fs::write(
            &large,
            format!("fn large() {{ let x = \"{}\"; }}", "x".repeat(4096)),
        )
        .unwrap();
        let config = Config {
            max_file_bytes: 1024,
            ..Config::default()
        };
        let mut index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 1);
        assert!(
            index.refresh().is_empty(),
            "unchanged refresh must be a no-op even with a skipped file"
        );
        assert!(index.refresh().is_empty());
        fs::write(&large, "fn large() {}").unwrap();
        let changed = index.refresh();
        assert!(changed.contains(&large));
        assert_eq!(index.files().len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_retried_after_permission_fix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("unreadable-retry");
        let path = dir.join("a.rs");
        fs::write(&path, "fn a() { let x = 1; }\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&path).is_ok() {
            // Running as root: permission bits are not enforced, skip.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            fs::remove_dir_all(&dir).ok();
            return;
        }
        let mut index = SourceIndex::build_with_cache(&dir, &Config::default(), None).unwrap();
        assert!(index.files().is_empty());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let changed = index.refresh();
        assert!(
            changed.contains(&path),
            "permission fix must be picked up without a reindex"
        );
        assert_eq!(index.files().len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_lives_in_project_root() {
        let dir = temp_dir("cache-default");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        let first = SourceIndex::build(&dir, &Config::default()).unwrap();
        assert_eq!(first.stats().parsed_files, 1);
        assert_eq!(entry_count(&dir.join(cache::CACHE_DIR)), 1);
        drop(first);
        let second = SourceIndex::build(&dir, &Config::default()).unwrap();
        assert_eq!(second.stats().cache_hits, 1);
        assert_eq!(second.files().len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_cache_location_writes_outside_project() {
        let dir = temp_dir("cache-user-location");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        let config = Config {
            cache_location: CacheLocation::UserCache,
            ..Config::default()
        };
        let first = SourceIndex::build(&dir, &config).unwrap();
        assert_eq!(first.stats().parsed_files, 1);
        assert!(!dir.join(cache::CACHE_DIR).exists());
        let root = dunce::canonicalize(&dir).unwrap();
        let Some(cache_dir) = cache::user_cache_dir(&root) else {
            fs::remove_dir_all(&dir).ok();
            return;
        };
        assert_eq!(entry_count(&cache_dir), 1);
        drop(first);
        let second = SourceIndex::build(&dir, &config).unwrap();
        assert_eq!(second.stats().cache_hits, 1);
        let cleared = SourceIndex::clear_cache(&dir);
        assert_eq!(cleared, vec![cache_dir.clone()]);
        assert!(!cache_dir.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_hits_avoid_reparsing() {
        let dir = temp_dir("cache-hit");
        let cache_dir = temp_dir("cache-hit-store");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; let y = 2; }\n").unwrap();
        let first =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(first.stats().cache_hits, 0);
        assert_eq!(first.stats().parsed_files, 1);
        let before: Vec<_> = first.files()[0]
            .tokens
            .iter()
            .map(|t| (t.kind, t.start, t.end))
            .collect();
        let hashes_before = first.files()[0].hashes.to_vec();
        drop(first);

        let second =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(second.stats().cache_hits, 1);
        assert_eq!(second.stats().parsed_files, 0);
        let after: Vec<_> = second.files()[0]
            .tokens
            .iter()
            .map(|t| (t.kind, t.start, t.end))
            .collect();
        assert_eq!(before, after);
        assert_eq!(
            hashes_before.as_slice(),
            second.files()[0].hashes.as_slice()
        );
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn cache_stores_one_entry_per_file_and_prunes_removed() {
        let dir = temp_dir("cache-entries");
        let cache_dir = temp_dir("cache-entries-store");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        let removed = dir.join("b.rs");
        fs::write(&removed, "fn b() { let y = 2; }\n").unwrap();
        let mut index =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(entry_count(&cache_dir), 2);
        fs::remove_file(&removed).unwrap();
        index.refresh();
        assert_eq!(index.files().len(), 1);
        assert_eq!(entry_count(&cache_dir), 1);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn one_shot_build_prunes_entries_of_removed_files_only() {
        let dir = temp_dir("one-shot-prune");
        let cache_dir = temp_dir("one-shot-prune-store");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        fs::write(dir.join("b.rs"), "fn b() { let y = 2; }\n").unwrap();
        fs::write(dir.join("c.py"), "def c():\n    return 1\n").unwrap();
        SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(entry_count(&cache_dir), 3);
        fs::remove_file(dir.join("b.rs")).unwrap();
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; let z = 3; }\n").unwrap();
        let rust_only = Config {
            languages: Some(vec![LanguageId::RUST]),
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &rust_only, Some(&cache_dir)).unwrap();
        assert_eq!(index.stats().parsed_files, 1);
        assert_eq!(entry_count(&cache_dir), 2);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn corrupt_token_offsets_fall_back_to_parsing() {
        let dir = temp_dir("cache-corrupt-tokens");
        let cache_dir = temp_dir("cache-corrupt-tokens-store");
        fs::write(dir.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        let first =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        drop(first);
        let root = dunce::canonicalize(&dir).unwrap();
        let path = root.join("a.rs");
        let entry = cache::entry_path(&cache_dir, &root, &path).expect("entry path");
        let mut bytes = fs::read(&entry).unwrap();
        let path_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let tokens_offset = 52 + path_len;
        assert_eq!(bytes[tokens_offset], 0, "first token starts at offset 0");
        bytes[tokens_offset + 1] = 0x7F;
        fs::write(&entry, bytes).unwrap();
        let second =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(second.stats().cache_hits, 0);
        assert_eq!(second.stats().parsed_files, 1);
        assert_eq!(second.files().len(), 1);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn cache_invalidates_changed_files() {
        let dir = temp_dir("cache-stale");
        let cache_dir = temp_dir("cache-stale-store");
        let file_path = dir.join("a.rs");
        fs::write(&file_path, "fn a() { let x = 1; }\n").unwrap();
        SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        fs::write(&file_path, "fn a() { let x = 1; let y = 2; let z = 3; }\n").unwrap();
        let rebuilt =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(rebuilt.stats().cache_hits, 0);
        assert_eq!(rebuilt.stats().parsed_files, 1);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn overlay_replaces_indexed_file_and_appends_new_file() {
        let source = "fn total(items: Vec<i32>) -> i32 {\n    let mut sum = 0;\n    for item in items {\n        sum = sum + item * item;\n        if sum > 100 {\n            sum = sum - 50;\n        }\n    }\n    sum\n}\n";
        let renamed = source
            .replace("total", "score")
            .replace("sum", "acc")
            .replace("item", "value");
        let dir = temp_dir("overlay");
        fs::write(dir.join("a.rs"), source).unwrap();
        fs::write(dir.join("b.rs"), "fn unrelated() {}\n").unwrap();
        let config = Config {
            min_lines: 5,
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        let overlay_at = |name: &str| {
            SourceFile::new(
                index.root().join(name),
                LanguageId::RUST,
                renamed.clone(),
                tokenize::tokenize(&renamed, LanguageId::RUST).unwrap(),
                None,
                renamed.len() as u64,
            )
        };

        let none = HashSet::new();
        let replaced = [overlay_at("b.rs")];
        let (files, positions) = index.files_with(&replaced, &none);
        assert_eq!(files.len(), index.files().len());
        assert!(std::ptr::eq(files[positions[0]], &replaced[0]));
        let span = [(positions[0], 0, replaced[0].tokens.len())];
        let groups = detect::detect_in_spans(&files, &span, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].occurrences.len(), 2);

        let appended = [overlay_at("c.rs")];
        let (files, positions) = index.files_with(&appended, &none);
        assert_eq!(files.len(), index.files().len() + 1);
        let span = [(positions[0], 0, appended[0].tokens.len())];
        assert_eq!(detect::detect_in_spans(&files, &span, &config).len(), 1);

        let moved = HashSet::from([index.root().join("a.rs")]);
        let (files, positions) = index.files_with(&appended, &moved);
        assert_eq!(files.len(), index.files().len());
        let span = [(positions[0], 0, appended[0].tokens.len())];
        assert!(detect::detect_in_spans(&files, &span, &config).is_empty());

        let dropped = HashSet::from([index.root().join("c.rs")]);
        let (files, positions) = index.files_with(&appended, &dropped);
        assert!(positions.is_empty());
        assert_eq!(files.len(), index.files().len());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn utf16_and_latin1_sources_are_indexed_and_cached() {
        let source = "fn total(items: Vec<i32>) -> i32 {\n    let mut sum = 0;\n    for item in items {\n        sum = sum + item * item;\n        if sum > 100 {\n            sum = sum - 50;\n        }\n    }\n    sum\n}\n";
        let renamed = source.replace("total", "score").replace("sum", "acc");
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in renamed.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let mut latin1 = b"int f(void) { const char *s = \"caf".to_vec();
        latin1.extend_from_slice(b"\xE9\"; return 0; }\n");
        let dir = temp_dir("encodings");
        let cache_dir = temp_dir("encodings-store");
        fs::write(dir.join("a.rs"), source).unwrap();
        fs::write(dir.join("b.rs"), utf16).unwrap();
        fs::write(dir.join("c.c"), latin1).unwrap();
        let config = Config {
            min_lines: 5,
            ..Config::default()
        };
        let first = SourceIndex::build_with_cache(&dir, &config, Some(&cache_dir)).unwrap();
        assert_eq!(first.files().len(), 3);
        let groups = first.find_clones(&config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].occurrences.len(), 2);
        drop(first);
        let second = SourceIndex::build_with_cache(&dir, &config, Some(&cache_dir)).unwrap();
        assert_eq!(second.stats().cache_hits, 3);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }

    #[test]
    fn concurrent_builds_share_one_cache() {
        let dir = temp_dir("concurrent");
        let cache_dir = temp_dir("concurrent-store");
        for index in 0..40 {
            fs::write(
                dir.join(format!("m{index}.rs")),
                format!("fn f{index}(a: i32) -> i32 {{\n    let b = a + {index};\n    b * 2\n}}\n"),
            )
            .unwrap();
        }
        type Snapshot = Vec<(PathBuf, Vec<(u8, u32, u32)>)>;
        let snapshot = |index: &SourceIndex| -> Snapshot {
            index
                .files()
                .iter()
                .map(|file| {
                    let tokens = file
                        .tokens
                        .iter()
                        .map(|t| (t.kind, t.start, t.end))
                        .collect();
                    (file.path.clone(), tokens)
                })
                .collect()
        };
        let results: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let index = SourceIndex::build_with_cache(
                            &dir,
                            &Config::default(),
                            Some(&cache_dir),
                        )
                        .unwrap();
                        snapshot(&index)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect()
        });
        assert!(results.iter().all(|result| result == &results[0]));
        assert_eq!(results[0].len(), 40);
        let warm =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(warm.stats().cache_hits, 40);
        assert_eq!(snapshot(&warm), results[0]);
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&cache_dir).ok();
    }
}
