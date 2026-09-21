use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::language::LanguageId;

pub const CONFIG_FILE_NAME: &str = "dup-detector.toml";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub min_lines: usize,
    pub min_occurrences: usize,
    pub max_bucket: usize,
    pub seed_window: usize,
    pub max_groups: Option<usize>,
    pub parameterize_literals: bool,
    pub languages: Vec<LanguageId>,
    pub no_ignore: bool,
    pub include_hidden: bool,
    pub max_file_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_lines: 7,
            min_occurrences: 2,
            max_bucket: 32,
            seed_window: 8,
            max_groups: None,
            parameterize_literals: false,
            languages: LanguageId::ALL.to_vec(),
            no_ignore: false,
            include_hidden: false,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("unknown language `{name}` in config file {path}")]
    UnknownLanguage { path: PathBuf, name: String },
    #[error("invalid value for `{key}` in config file {path}: {message}")]
    InvalidValue {
        path: PathBuf,
        key: &'static str,
        message: String,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    min_lines: Option<usize>,
    min_occurrences: Option<usize>,
    max_bucket: Option<usize>,
    seed_window: Option<usize>,
    max_groups: Option<usize>,
    parameterize_literals: Option<bool>,
    languages: Option<Vec<String>>,
    include_hidden: Option<bool>,
    max_file_bytes: Option<u64>,
}

fn find_config_file(root: &Path) -> Option<PathBuf> {
    // Ancestor search only walks real directory components, so relative inputs
    // like `.` or `src` must be absolutized first (canonicalize also resolves
    // `..` and symlinks).
    let root = match root.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => {
            if root.is_absolute() {
                root.to_path_buf()
            } else {
                std::env::current_dir().ok()?.join(root)
            }
        }
    };
    let start = if root.is_dir() {
        root
    } else {
        root.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    start
        .ancestors()
        .map(|dir| dir.join(CONFIG_FILE_NAME))
        .find(|candidate| candidate.is_file())
}

impl Config {
    /// Load the project config by searching for `dup-detector.toml` from `root`
    /// upwards to the filesystem root.
    ///
    /// Returns `Ok(None)` when no config file is found. `root` may be a file, in
    /// which case the search starts at its parent directory.
    pub fn load(root: impl AsRef<Path>) -> Result<Option<Self>, ConfigError> {
        let root = root.as_ref();
        let Some(path) = find_config_file(root) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        Self::from_toml(&text, &path).map(Some)
    }

    pub fn load_or_default(root: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Ok(Self::load(root)?.unwrap_or_default())
    }

    pub fn from_toml(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config = Self::default();
        if let Some(value) = file.min_lines {
            config.min_lines = value;
        }
        if let Some(value) = file.min_occurrences {
            config.min_occurrences = value;
        }
        if let Some(value) = file.max_bucket {
            config.max_bucket = value;
        }
        if let Some(value) = file.seed_window {
            config.seed_window = value;
        }
        if let Some(value) = file.max_groups {
            config.max_groups = Some(value);
        }
        if let Some(value) = file.parameterize_literals {
            config.parameterize_literals = value;
        }
        if let Some(value) = file.max_file_bytes {
            config.max_file_bytes = value;
        }
        if let Some(value) = file.include_hidden {
            config.include_hidden = value;
        }
        if let Some(names) = file.languages {
            let mut languages = Vec::with_capacity(names.len());
            for name in names {
                let language =
                    LanguageId::from_name(&name).ok_or_else(|| ConfigError::UnknownLanguage {
                        path: path.to_path_buf(),
                        name: name.clone(),
                    })?;
                languages.push(language);
            }
            config.languages = languages;
        }
        let invalid = |key: &'static str, message: &str| ConfigError::InvalidValue {
            path: path.to_path_buf(),
            key,
            message: message.to_string(),
        };
        if config.min_lines == 0 {
            return Err(invalid("min_lines", "must be at least 1"));
        }
        if config.min_occurrences == 0 {
            return Err(invalid("min_occurrences", "must be at least 1"));
        }
        if config.max_bucket == 0 {
            return Err(invalid("max_bucket", "must be at least 1"));
        }
        if config.seed_window == 0 {
            return Err(invalid("seed_window", "must be at least 1"));
        }
        if config.max_file_bytes == 0 {
            return Err(invalid("max_file_bytes", "must be at least 1"));
        }
        if config.max_file_bytes > u32::MAX as u64 {
            return Err(invalid(
                "max_file_bytes",
                "exceeds the 4 GiB token-offset limit",
            ));
        }
        Ok(config)
    }

    pub fn language_enabled(&self, language: LanguageId) -> bool {
        self.languages.contains(&language)
    }

    pub fn with_limits(
        mut self,
        min_lines: Option<usize>,
        min_occurrences: Option<usize>,
        max_groups: Option<usize>,
    ) -> Self {
        if let Some(v) = min_lines {
            self.min_lines = v;
        }
        if let Some(v) = min_occurrences {
            self.min_occurrences = v;
        }
        if let Some(v) = max_groups {
            self.max_groups = Some(v);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parses_full_config() {
        let text = r#"
            min_lines = 10
            min_occurrences = 3
            max_bucket = 16
            seed_window = 6
            max_groups = 25
            parameterize_literals = true
            languages = ["rust", "python"]
            include_hidden = true
            max_file_bytes = 1048576
        "#;
        let config = Config::from_toml(text, Path::new(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(
            config,
            Config {
                min_lines: 10,
                min_occurrences: 3,
                max_bucket: 16,
                seed_window: 6,
                max_groups: Some(25),
                parameterize_literals: true,
                languages: vec![LanguageId::Rust, LanguageId::Python],
                no_ignore: false,
                include_hidden: true,
                max_file_bytes: 1048576,
            }
        );
    }

    #[test]
    fn rejects_degenerate_values() {
        for text in [
            "min_lines = 0",
            "min_occurrences = 0",
            "max_bucket = 0",
            "seed_window = 0",
            "max_file_bytes = 0",
            "max_file_bytes = 4294967296",
        ] {
            let error = Config::from_toml(text, Path::new(CONFIG_FILE_NAME));
            assert!(
                matches!(error, Err(ConfigError::InvalidValue { .. })),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let config = Config::from_toml("min_lines = 12\n", Path::new(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(config.min_lines, 12);
        assert_eq!(config.min_occurrences, Config::default().min_occurrences);
        assert_eq!(config.languages, Config::default().languages);
    }

    #[test]
    fn rejects_unknown_language() {
        let error = Config::from_toml(
            "languages = [\"rust\", \"cobol\"]\n",
            Path::new(CONFIG_FILE_NAME),
        );
        assert!(matches!(error, Err(ConfigError::UnknownLanguage { .. })));
    }

    #[test]
    fn rejects_unknown_key() {
        let error = Config::from_toml("min_line = 5\n", Path::new(CONFIG_FILE_NAME));
        assert!(matches!(error, Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn missing_file_yields_none() {
        let dir = std::env::temp_dir().join(format!("dup-detector-config-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), None);
        assert_eq!(Config::load_or_default(&dir).unwrap(), Config::default());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_from_project_root() {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-config-root-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(CONFIG_FILE_NAME), "min_lines = 7\n").unwrap();
        let config = Config::load(&dir).unwrap().unwrap();
        assert_eq!(config.min_lines, 7);
        let from_file = Config::load(dir.join("src.rs")).unwrap().unwrap();
        assert_eq!(from_file.min_lines, 7);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_config_in_ancestor_directory() {
        let dir = std::env::temp_dir().join(format!(
            "dup-detector-config-ancestor-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        let nested = dir.join("src").join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join(CONFIG_FILE_NAME), "min_lines = 9\n").unwrap();
        let config = Config::load(&nested).unwrap().unwrap();
        assert_eq!(config.min_lines, 9);
        fs::remove_dir_all(&dir).ok();
    }
}
