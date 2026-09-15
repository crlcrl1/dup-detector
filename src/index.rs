use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use rayon::prelude::*;
use thiserror::Error;

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

pub struct SourceIndex {
    root: PathBuf,
    config: Config,
    files: Vec<SourceFile>,
    by_path: HashMap<PathBuf, usize>,
}

impl SourceIndex {
    pub fn build(root: impl AsRef<Path>, config: &Config) -> Result<Self, IndexError> {
        let root = root.as_ref().canonicalize().map_err(|e| IndexError::Io {
            path: root.as_ref().to_path_buf(),
            source: e,
        })?;
        let paths = discover(&root, config)?;
        let files = parse_many(&paths);
        Ok(Self::from_parts(root, config.clone(), files))
    }

    pub fn refresh(&mut self) {
        let paths = match discover(&self.root, &self.config) {
            Ok(paths) => paths,
            Err(e) => {
                tracing::warn!(error = %e, "refresh: file discovery failed");
                return;
            }
        };
        let mut old: HashMap<PathBuf, SourceFile> = std::mem::take(&mut self.files)
            .into_iter()
            .map(|f| (f.path.clone(), f))
            .collect();
        let mut files = Vec::with_capacity(paths.len());
        for (path, language) in paths {
            if let Some(f) = old.remove(&path)
                && let Ok(meta) = fs::metadata(&path)
                && f.modified == meta.modified().ok()
                && f.size == meta.len()
            {
                files.push(f);
                continue;
            }
            if let Some(f) = parse_file(&path, language) {
                files.push(f);
            }
        }
        self.files = files;
        self.by_path = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn file_by_path(&self, path: &Path) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub fn find_clones(&self, config: &Config) -> Vec<CloneGroup> {
        let files: Vec<&SourceFile> = self.files.iter().collect();
        detect::detect(&files, config)
    }

    fn from_parts(root: PathBuf, config: Config, files: Vec<SourceFile>) -> Self {
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
        }
    }
}

fn discover(root: &Path, config: &Config) -> Result<Vec<(PathBuf, LanguageId)>, IndexError> {
    let walker = WalkBuilder::new(root).build();
    let mut paths = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|e| IndexError::Walk {
            root: root.to_path_buf(),
            source: e,
        })?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(language) = LanguageId::from_path(path) else {
            continue;
        };
        if config.language_enabled(language) {
            paths.push((path.to_path_buf(), language));
        }
    }
    paths.sort();
    Ok(paths)
}

fn parse_many(paths: &[(PathBuf, LanguageId)]) -> Vec<SourceFile> {
    paths
        .par_iter()
        .filter_map(|(path, language)| parse_file(path, *language))
        .collect()
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
    Some(SourceFile {
        path: path.to_path_buf(),
        language,
        text,
        tokens,
        modified: meta.modified().ok(),
        size: meta.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-test-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
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
        let index = SourceIndex::build(&dir, &config).unwrap();
        assert_eq!(index.files().len(), 1);
        assert_eq!(index.files()[0].path.file_name().unwrap(), "a.rs");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refresh_reparses_changed_files() {
        let dir = temp_dir("refresh");
        let file_path = dir.join("a.rs");
        fs::write(&file_path, "fn a() { let x = 1; }\n").unwrap();
        let mut index = SourceIndex::build(&dir, &Config::default()).unwrap();
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
}
