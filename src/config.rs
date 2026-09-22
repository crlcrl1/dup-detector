use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use thiserror::Error;

use crate::language::LanguageId;
use crate::model::CloneType;

pub const CONFIG_FILE_NAME: &str = "dup-detector.toml";
pub const CONFIG_DIR_NAME: &str = "dup-detector";

/// Where per-file token caches are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheLocation {
    /// `<project root>/.dup-detector`, the default.
    #[default]
    Project,
    /// A per-root directory under the user's platform cache directory.
    UserCache,
}

impl CacheLocation {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "project" => Some(Self::Project),
            "user-cache" => Some(Self::UserCache),
            _ => None,
        }
    }
}

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
    /// Drop clone groups whose occurrences look like pure declarations
    /// (imports, struct fields, ...) with no logic markers.
    pub filter_boilerplate: bool,
    /// Directory for the on-disk token cache.
    pub cache_location: CacheLocation,
    /// Per-request clone type filter; applied during detection so that
    /// `max_groups` truncation happens after filtering. Not settable from the
    /// config file.
    pub types: Option<Vec<CloneType>>,
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
            filter_boilerplate: true,
            cache_location: CacheLocation::Project,
            types: None,
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
    #[error("invalid value for parameter `{key}`: {message}")]
    InvalidParameter { key: &'static str, message: String },
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
    no_ignore: Option<bool>,
    include_hidden: Option<bool>,
    max_file_bytes: Option<u64>,
    filter_boilerplate: Option<bool>,
    cache_location: Option<String>,
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

/// Path of the user-level config file: `~/.config/dup-detector/dup-detector.toml`
/// on Linux, `~/Library/Application Support/dup-detector/dup-detector.toml` on
/// macOS and `%APPDATA%\dup-detector\dup-detector.toml` on Windows.
pub fn user_config_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME),
    )
}

fn source_paths_layered(root: &Path, user: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = user.filter(|path| path.is_file()) {
        paths.push(path.to_path_buf());
    }
    if let Some(path) = find_config_file(root) {
        paths.push(path);
    }
    paths
}

fn read_file_config(path: &Path) -> Result<FileConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn apply_file_config(
    config: &mut Config,
    file: FileConfig,
    path: &Path,
) -> Result<(), ConfigError> {
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
    if let Some(value) = file.no_ignore {
        config.no_ignore = value;
    }
    if let Some(value) = file.filter_boilerplate {
        config.filter_boilerplate = value;
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
    if let Some(name) = file.cache_location {
        config.cache_location = CacheLocation::from_name(&name)
            .ok_or_else(|| invalid("cache_location", "expected `project` or `user-cache`"))?;
    }
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
    Ok(())
}

impl Config {
    /// Load the effective configuration for `root`: built-in defaults,
    /// overridden by the user-level config file, overridden by the nearest
    /// project-level `dup-detector.toml`.
    ///
    /// Returns `Ok(None)` when neither config file exists. `root` may be a file,
    /// in which case the project search starts at its parent directory.
    pub fn load(root: impl AsRef<Path>) -> Result<Option<Self>, ConfigError> {
        Self::load_layered(root, user_config_path().as_deref())
    }

    /// Like [`Config::load`], but with an explicit user-level config path.
    pub fn load_layered(
        root: impl AsRef<Path>,
        user: Option<&Path>,
    ) -> Result<Option<Self>, ConfigError> {
        let user = user.filter(|path| path.is_file());
        let project = find_config_file(root.as_ref());
        if user.is_none() && project.is_none() {
            return Ok(None);
        }
        let mut config = Self::default();
        if let Some(path) = user {
            apply_file_config(&mut config, read_file_config(path)?, path)?;
        }
        if let Some(path) = project {
            apply_file_config(&mut config, read_file_config(&path)?, &path)?;
        }
        Ok(Some(config))
    }

    /// Existing config files for `root`, in increasing priority: the user-level
    /// file first, then the nearest project-level file.
    pub fn source_paths(root: impl AsRef<Path>) -> Vec<PathBuf> {
        source_paths_layered(root.as_ref(), user_config_path().as_deref())
    }

    /// [`Config::source_paths`] paired with each file's mtime, so long-running
    /// servers can cheaply detect created, removed and edited config files.
    pub fn source_stamps(root: impl AsRef<Path>) -> Vec<(PathBuf, Option<SystemTime>)> {
        Self::source_paths(root)
            .into_iter()
            .map(|path| {
                let mtime = std::fs::metadata(&path)
                    .ok()
                    .and_then(|meta| meta.modified().ok());
                (path, mtime)
            })
            .collect()
    }

    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let file = read_file_config(path)?;
        let mut config = Self::default();
        apply_file_config(&mut config, file, path)?;
        Ok(config)
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
        apply_file_config(&mut config, file, path)?;
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
    ) -> Result<Self, ConfigError> {
        let invalid = |key: &'static str, message: &str| ConfigError::InvalidParameter {
            key,
            message: message.to_string(),
        };
        if let Some(v) = min_lines {
            if v == 0 {
                return Err(invalid("min_lines", "must be at least 1"));
            }
            self.min_lines = v;
        }
        if let Some(v) = min_occurrences {
            if v == 0 {
                return Err(invalid("min_occurrences", "must be at least 1"));
            }
            self.min_occurrences = v;
        }
        if let Some(v) = max_groups {
            self.max_groups = Some(v);
        }
        Ok(self)
    }

    pub fn with_max_file_bytes(mut self, max_file_bytes: u64) -> Result<Self, ConfigError> {
        if max_file_bytes == 0 {
            return Err(ConfigError::InvalidParameter {
                key: "max_file_bytes",
                message: "must be at least 1".to_string(),
            });
        }
        if max_file_bytes > u32::MAX as u64 {
            return Err(ConfigError::InvalidParameter {
                key: "max_file_bytes",
                message: "exceeds the 4 GiB token-offset limit".to_string(),
            });
        }
        self.max_file_bytes = max_file_bytes;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-config-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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
            no_ignore = true
            include_hidden = true
            max_file_bytes = 1048576
            filter_boilerplate = false
            cache_location = "user-cache"
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
                no_ignore: true,
                include_hidden: true,
                max_file_bytes: 1048576,
                filter_boilerplate: false,
                cache_location: CacheLocation::UserCache,
                types: None,
            }
        );
    }

    #[test]
    fn parses_cache_location() {
        let user = Config::from_toml(
            "cache_location = \"user-cache\"\n",
            Path::new(CONFIG_FILE_NAME),
        )
        .unwrap();
        assert_eq!(user.cache_location, CacheLocation::UserCache);
        let project = Config::from_toml(
            "cache_location = \"project\"\n",
            Path::new(CONFIG_FILE_NAME),
        )
        .unwrap();
        assert_eq!(project.cache_location, CacheLocation::Project);
        let error = Config::from_toml(
            "cache_location = \"elsewhere\"\n",
            Path::new(CONFIG_FILE_NAME),
        );
        assert!(
            matches!(
                error,
                Err(ConfigError::InvalidValue {
                    key: "cache_location",
                    ..
                })
            ),
            "unknown cache location must be rejected"
        );
    }

    #[test]
    fn project_config_overrides_user_config() {
        let dir = temp_dir("layered");
        let project = dir.join("project");
        let user = dir.join("user");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&user).unwrap();
        let user_config = user.join(CONFIG_FILE_NAME);
        fs::write(
            &user_config,
            "min_lines = 9\nmax_bucket = 64\ncache_location = \"user-cache\"\n",
        )
        .unwrap();
        let project_config = project.join(CONFIG_FILE_NAME);
        fs::write(&project_config, "min_lines = 4\n").unwrap();
        let config = Config::load_layered(&project, Some(&user_config))
            .unwrap()
            .unwrap();
        assert_eq!(config.min_lines, 4, "project config wins");
        assert_eq!(
            config.max_bucket, 64,
            "user config fills the remaining keys"
        );
        assert_eq!(config.cache_location, CacheLocation::UserCache);
        assert_eq!(
            source_paths_layered(&project, Some(&user_config)),
            vec![user_config, project_config]
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_config_applies_without_project_config() {
        let dir = temp_dir("user-only");
        let user = dir.join("user");
        fs::create_dir_all(&user).unwrap();
        let user_config = user.join(CONFIG_FILE_NAME);
        fs::write(&user_config, "min_lines = 3\n").unwrap();
        let config = Config::load_layered(&dir, Some(&user_config))
            .unwrap()
            .unwrap();
        assert_eq!(config.min_lines, 3);
        assert_eq!(config.min_occurrences, Config::default().min_occurrences);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignored_missing_user_config() {
        let dir = temp_dir("user-missing");
        let user_config = dir.join("nope.toml");
        assert_eq!(
            Config::load_layered(&dir, Some(&user_config)).unwrap(),
            None
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_limits_rejects_degenerate_overrides() {
        let config = Config::default();
        assert!(matches!(
            config.clone().with_limits(Some(0), None, None),
            Err(ConfigError::InvalidParameter {
                key: "min_lines",
                ..
            })
        ));
        assert!(matches!(
            config.clone().with_limits(None, Some(0), None),
            Err(ConfigError::InvalidParameter {
                key: "min_occurrences",
                ..
            })
        ));
        assert!(matches!(
            config.clone().with_max_file_bytes(0),
            Err(ConfigError::InvalidParameter {
                key: "max_file_bytes",
                ..
            })
        ));
        assert!(matches!(
            config.clone().with_max_file_bytes(u32::MAX as u64 + 1),
            Err(ConfigError::InvalidParameter {
                key: "max_file_bytes",
                ..
            })
        ));
        assert_eq!(
            config
                .clone()
                .with_limits(Some(3), Some(4), Some(5))
                .unwrap(),
            Config {
                min_lines: 3,
                min_occurrences: 4,
                max_groups: Some(5),
                ..config.clone()
            }
        );
        assert_eq!(
            config.with_max_file_bytes(1024).unwrap().max_file_bytes,
            1024
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
        let dir = temp_dir("none");
        assert_eq!(Config::load_layered(&dir, None).unwrap(), None);
        assert_eq!(
            Config::load_layered(&dir, None)
                .unwrap()
                .unwrap_or_default(),
            Config::default()
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_from_project_root() {
        let dir = temp_dir("root");
        fs::write(dir.join(CONFIG_FILE_NAME), "min_lines = 7\n").unwrap();
        let config = Config::load_layered(&dir, None).unwrap().unwrap();
        assert_eq!(config.min_lines, 7);
        let from_file = Config::load_layered(dir.join("src.rs"), None)
            .unwrap()
            .unwrap();
        assert_eq!(from_file.min_lines, 7);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_config_in_ancestor_directory() {
        let dir = temp_dir("ancestor");
        let nested = dir.join("src").join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join(CONFIG_FILE_NAME), "min_lines = 9\n").unwrap();
        let config = Config::load_layered(&nested, None).unwrap().unwrap();
        assert_eq!(config.min_lines, 9);
        fs::remove_dir_all(&dir).ok();
    }
}
