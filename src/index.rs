use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
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

pub struct SourceIndex {
    root: PathBuf,
    config: Config,
    files: Vec<SourceFile>,
    by_path: HashMap<PathBuf, usize>,
    cache_dir: Option<PathBuf>,
    stats: BuildStats,
}

impl SourceIndex {
    pub fn build(root: impl AsRef<Path>, config: &Config) -> Result<Self, IndexError> {
        let root = canonicalize_root(root)?;
        let cache_dir = cache::dir_in(&root);
        Self::build_at(root, config, Some(cache_dir))
    }

    pub fn build_with_cache(
        root: impl AsRef<Path>,
        config: &Config,
        cache_dir: Option<&Path>,
    ) -> Result<Self, IndexError> {
        let root = canonicalize_root(root)?;
        Self::build_at(root, config, cache_dir.map(Path::to_path_buf))
    }

    fn build_at(
        root: PathBuf,
        config: &Config,
        cache_dir: Option<PathBuf>,
    ) -> Result<Self, IndexError> {
        let paths = discover(&root, config)?;
        let mut slots: Vec<Option<SourceFile>> = paths
            .par_iter()
            .map(|(path, language)| {
                cache_dir
                    .as_deref()
                    .and_then(|dir| file_from_cache(dir, &root, path, *language))
            })
            .collect();
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
                let file = parse_file(&paths[index].0, paths[index].1);
                if let Some(file) = &file
                    && let Some(dir) = &cache_dir
                    && let Err(error) = cache::store_entry(dir, &root, file)
                {
                    tracing::debug!(error = %error, "cannot write cache entry");
                }
                file
            })
            .collect();
        for (slot, file) in misses.into_iter().zip(parsed) {
            slots[slot] = file;
        }
        let files: Vec<SourceFile> = slots.into_iter().flatten().collect();
        let stats = BuildStats {
            cache_hits,
            parsed_files,
        };
        let index = Self::from_parts(root, config.clone(), files, cache_dir, stats);
        tracing::debug!(
            cache_hits = index.stats.cache_hits,
            parsed_files = index.stats.parsed_files,
            files = index.files.len(),
            "index built"
        );
        Ok(index)
    }

    pub fn clear_cache(root: &Path) {
        let Ok(root) = root.canonicalize() else {
            return;
        };
        if let Err(error) = cache::clear(&cache::dir_in(&root)) {
            tracing::debug!(root = %root.display(), error = %error, "cannot clear cache");
        }
    }

    pub fn refresh(&mut self) -> Vec<PathBuf> {
        let paths = match discover(&self.root, &self.config) {
            Ok(paths) => paths,
            Err(e) => {
                tracing::warn!(error = %e, "refresh: file discovery failed");
                return Vec::new();
            }
        };
        if paths.len() == self.files.len()
            && paths.par_iter().all(|(path, _)| {
                self.by_path.get(path).is_some_and(|&slot| {
                    let file = &self.files[slot];
                    fs::metadata(path).is_ok_and(|meta| {
                        file.modified == meta.modified().ok() && file.size == meta.len()
                    })
                })
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
            .map(|(path, _)| {
                old.get(path).is_some_and(|f| {
                    fs::metadata(path).is_ok_and(|meta| {
                        f.modified == meta.modified().ok() && f.size == meta.len()
                    })
                })
            })
            .collect();
        let mut old = old;
        let mut slots: Vec<Option<SourceFile>> = Vec::with_capacity(paths.len());
        let mut changed = Vec::new();
        let mut misses: Vec<usize> = Vec::new();
        for (index, (path, _)) in paths.iter().enumerate() {
            let existing = old.remove(path);
            if reusable[index] {
                slots.push(existing);
                continue;
            }
            misses.push(index);
            changed.push(path.clone());
            slots.push(None);
        }
        let parsed_count = misses.len();
        let parsed: Vec<Option<SourceFile>> = misses
            .par_iter()
            .map(|&index| {
                let file = parse_file(&paths[index].0, paths[index].1);
                if let Some(file) = &file
                    && let Some(dir) = &self.cache_dir
                    && let Err(error) = cache::store_entry(dir, &self.root, file)
                {
                    tracing::debug!(error = %error, "cannot write cache entry");
                }
                file
            })
            .collect();
        for (slot, file) in misses.into_iter().zip(parsed) {
            slots[slot] = file;
        }
        if let Some(dir) = &self.cache_dir {
            for removed in old.values() {
                cache::remove_entry(dir, &self.root, &removed.path);
            }
        }
        changed.extend(old.into_keys());
        self.files = slots.into_iter().flatten().collect();
        self.by_path = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        self.stats = BuildStats {
            cache_hits: 0,
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

    fn from_parts(
        root: PathBuf,
        config: Config,
        files: Vec<SourceFile>,
        cache_dir: Option<PathBuf>,
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
            cache_dir,
            stats,
        }
    }
}

fn canonicalize_root(root: impl AsRef<Path>) -> Result<PathBuf, IndexError> {
    root.as_ref().canonicalize().map_err(|e| IndexError::Io {
        path: root.as_ref().to_path_buf(),
        source: e,
    })
}

fn file_from_cache(
    dir: &Path,
    root: &Path,
    path: &Path,
    language: LanguageId,
) -> Option<SourceFile> {
    let entry = cache::load_entry(&cache::entry_path(dir, root, path)?)?;
    if entry.path != path.strip_prefix(root).ok()? {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    if meta.len() != entry.size || meta.modified().ok() != Some(entry.modified) {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    if cache::text_hash(&text) != entry.text_hash {
        return None;
    }
    Some(SourceFile::with_hashes(
        path.to_path_buf(),
        language,
        text,
        entry.tokens,
        entry.hashes,
        Some(entry.modified),
        entry.size,
    ))
}

fn discover(root: &Path, config: &Config) -> Result<Vec<(PathBuf, LanguageId)>, IndexError> {
    let mut paths = Vec::new();
    let mut failure = None;
    for entry in WalkBuilder::new(root)
        .filter_entry(|entry| entry.file_name() != cache::CACHE_DIR)
        .build()
    {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && let Some(language) = LanguageId::from_path(entry.path())
                    && config.language_enabled(language)
                {
                    paths.push((entry.path().to_path_buf(), language));
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if let Some(source) = failure {
        return Err(IndexError::Walk {
            root: root.to_path_buf(),
            source,
        });
    }
    paths.sort();
    Ok(paths)
}

pub(crate) fn parse_file(path: &Path, language: LanguageId) -> Option<SourceFile> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot stat file");
            return None;
        }
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read file");
            return None;
        }
    };
    let tokens = match tokenize::tokenize(&text, language) {
        Ok(tokens) => tokens,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot tokenize");
            return None;
        }
    };
    Some(SourceFile::new(
        path.to_path_buf(),
        language,
        text,
        tokens,
        meta.modified().ok(),
        meta.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-test-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry_count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    #[test]
    fn discovers_supported_files_only() {
        let dir = temp_dir("discover");
        fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        fs::write(dir.join("b.py"), "def b(): pass").unwrap();
        fs::write(dir.join("notes.txt"), "not code").unwrap();
        let config = Config {
            languages: vec![LanguageId::Rust],
            ..Config::default()
        };
        let index = SourceIndex::build_with_cache(&dir, &config, None).unwrap();
        assert_eq!(index.files().len(), 1);
        assert_eq!(index.files()[0].path.file_name().unwrap(), "a.rs");
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
            .map(|t| (t.kind, t.start, t.end, t.line))
            .collect();
        drop(first);

        let second =
            SourceIndex::build_with_cache(&dir, &Config::default(), Some(&cache_dir)).unwrap();
        assert_eq!(second.stats().cache_hits, 1);
        assert_eq!(second.stats().parsed_files, 0);
        let after: Vec<_> = second.files()[0]
            .tokens
            .iter()
            .map(|t| (t.kind, t.start, t.end, t.line))
            .collect();
        assert_eq!(before, after);
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
}
