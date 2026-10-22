//! Runtime language plugins: tree-sitter grammars loaded from shared
//! libraries described by a small manifest, without recompiling dup-detector.
//!
//! A plugin is a directory inside the user-level grammars directory
//! (`~/.config/dup-detector/grammars` on Linux) containing:
//!
//! - one shared library (`*.so` / `*.dll` / `*.dylib`) exporting the
//!   grammar's `tree_sitter_<name>` symbol, and
//! - a `grammar.toml` manifest:
//!
//! ```toml
//! name = "go"                    # optional, defaults to the directory name
//! extensions = ["go"]
//! library = "tree-sitter-go.so"  # optional when the directory holds exactly one
//! symbol = "tree_sitter_go"      # optional, defaults to tree_sitter_<name>
//! identifier_kinds = [...]       # optional additive classification overrides
//! literal_kinds = [...]
//! unit_kinds = [...]
//! container_kinds = [...]
//! ```
//!
//! Plugins are native code loaded into the process: only install grammars
//! from sources you trust. A broken plugin is logged and skipped; it never
//! prevents built-in languages or other plugins from loading.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use tracing::warn;
use tree_sitter_language::LanguageFn;

use crate::language::{KindOverrides, LanguageRegistry, LanguageSource, LanguageSpec, registry};

/// Manifest file name inside a plugin directory.
pub const MANIFEST_FILE_NAME: &str = "grammar.toml";
/// Name of the directory inside the user-level config directory that holds
/// grammar plugins.
pub const GRAMMARS_DIR_NAME: &str = "grammars";

/// User-level grammar plugin directory: `~/.config/dup-detector/grammars` on
/// Linux, `~/Library/Application Support/dup-detector/grammars` on macOS and
/// `%APPDATA%\dup-detector\grammars` on Windows.
pub fn user_grammars_dir() -> Option<PathBuf> {
    crate::config::user_config_dir().map(|dir| dir.join(GRAMMARS_DIR_NAME))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PluginManifest {
    name: Option<String>,
    extensions: Vec<String>,
    library: Option<String>,
    symbol: Option<String>,
    identifier_kinds: Vec<String>,
    literal_kinds: Vec<String>,
    unit_kinds: Vec<String>,
    container_kinds: Vec<String>,
}

/// Why a single plugin failed to load. Loading is best-effort: failures are
/// logged and the plugin is skipped.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("manifest is missing `extensions`")]
    NoExtensions,
    #[error("no shared library found (expected one of *.so/*.dll/*.dylib or a `library` key)")]
    NoLibrary,
    #[error("multiple shared libraries found; set `library` in the manifest")]
    AmbiguousLibrary,
    #[error("grammar symbol `{0}` not found in the library")]
    Symbol(String),
    #[error("grammar ABI version {0} is not supported (need >= {MIN_ABI_VERSION})")]
    Abi(u64),
    #[error("language is already registered: {0}")]
    Duplicate(#[from] crate::language::RegisterError),
}

/// A plugin that loaded successfully.
#[derive(Debug)]
pub struct LoadedPlugin {
    pub name: String,
    pub dir: PathBuf,
}

/// Load every plugin directory inside `dir` into `registry`. Each failure is
/// logged and skipped; loading one plugin never affects the others.
pub fn load_plugins_from(dir: &Path, registry: &mut LanguageRegistry) -> Vec<LoadedPlugin> {
    let mut loaded = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warn!("cannot scan grammar plugins in {}: {error}", dir.display());
            return loaded;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match load_plugin(&path, registry) {
            Ok(name) => loaded.push(LoadedPlugin { name, dir: path }),
            Err(error) => warn!("skipping grammar plugin {}: {error}", path.display()),
        }
    }
    loaded
}

/// Load plugins from the user-level grammars directory into the global
/// registry. No-op when the directory does not exist.
pub fn load_user_plugins() -> Vec<LoadedPlugin> {
    let Some(dir) = user_grammars_dir() else {
        return Vec::new();
    };
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut registry = registry().write().expect("language registry poisoned");
    load_plugins_from(&dir, &mut registry)
}

/// Outcome of a successful plugin health check.
pub struct PluginHealth {
    pub name: String,
    pub extensions: Vec<String>,
    pub abi_version: u64,
}

/// Validate one plugin directory (manifest, library, symbol, ABI, kind
/// tables) by loading it into a scratch registry. Used by `dup-detector
/// doctor`; startup loading stays best-effort and only warns.
pub fn check_plugin(dir: &Path) -> Result<PluginHealth, PluginError> {
    let mut scratch = LanguageRegistry::default();
    let name = load_plugin(dir, &mut scratch)?;
    let (_, def) = scratch
        .iter()
        .find(|(_, def)| def.name() == name)
        .expect("load_plugin registers the language");
    Ok(PluginHealth {
        abi_version: def.grammar().abi_version() as u64,
        name: def.name().to_string(),
        extensions: def.extensions().to_vec(),
    })
}

fn load_plugin(dir: &Path, registry: &mut LanguageRegistry) -> Result<String, PluginError> {
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let text = std::fs::read_to_string(&manifest_path).map_err(|source| PluginError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest: PluginManifest = toml::from_str(&text).map_err(|source| PluginError::Parse {
        path: manifest_path,
        source,
    })?;
    if manifest.extensions.is_empty() {
        return Err(PluginError::NoExtensions);
    }
    let name = match manifest.name {
        Some(ref name) => name.clone(),
        None => dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unnamed")
            .to_string(),
    };
    let library_path = resolve_library(dir, &manifest)?;
    let symbol = manifest
        .symbol
        .clone()
        .unwrap_or_else(|| format!("tree_sitter_{}", name.replace('-', "_")));
    // SAFETY: the library is trusted native code per the module docs; it stays
    // mapped for the registry's lifetime via `LanguageSpec::library`.
    let library =
        unsafe { libloading::Library::new(&library_path).map_err(|_| PluginError::NoLibrary)? };
    // SAFETY: the symbol has the standard tree-sitter grammar signature; the
    // returned raw pointer is only used to construct `Language` values while
    // the library remains loaded.
    let grammar_fn = unsafe {
        library
            .get::<LanguageFn>(symbol.as_bytes())
            .map_err(|_| PluginError::Symbol(symbol.clone()))
    }?;
    let grammar = tree_sitter::Language::from(*grammar_fn);
    let abi_version = language_abi_version(&grammar);
    if abi_version < MIN_ABI_VERSION {
        return Err(PluginError::Abi(abi_version));
    }
    registry.register(LanguageSpec {
        name: name.clone(),
        aliases: Vec::new(),
        extensions: manifest.extensions.clone(),
        grammar,
        kind_overrides: KindOverrides {
            identifier: manifest.identifier_kinds,
            literal: manifest.literal_kinds,
            unit: manifest.unit_kinds,
            container: manifest.container_kinds,
        },
        source: LanguageSource::Plugin {
            dir: dir.to_path_buf(),
        },
        library: Some(library),
    })?;
    Ok(name)
}

/// Oldest tree-sitter ABI this build can parse.
const MIN_ABI_VERSION: u64 = 13;

fn language_abi_version(language: &tree_sitter::Language) -> u64 {
    language.abi_version() as u64
}

fn resolve_library(dir: &Path, manifest: &PluginManifest) -> Result<PathBuf, PluginError> {
    if let Some(library) = &manifest.library {
        let path = dir.join(library);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(PluginError::NoLibrary)
        };
    }
    let mut candidates = std::fs::read_dir(dir)
        .map_err(|source| PluginError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "so" | "dll" | "dylib"))
        });
    let first = candidates.next();
    match (first, candidates.next()) {
        (Some(path), None) => Ok(path),
        (Some(_), Some(_)) => Err(PluginError::AmbiguousLibrary),
        _ => Err(PluginError::NoLibrary),
    }
}
