//! `dup-detector install` support: fetch a tree-sitter grammar repository,
//! compile it into a shared library and install it as a language plugin under
//! the user-level grammars directory.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use thiserror::Error;

use crate::cache;
use crate::plugin::{MANIFEST_FILE_NAME, user_grammars_dir};

/// Why installing a grammar failed.
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("git is required to download grammars but was not found in PATH")]
    GitMissing,
    #[error("a C compiler is required to build grammars but was not found in PATH")]
    CompilerMissing,
    #[error("cannot download {url}: {message}")]
    Download { url: String, message: String },
    #[error("compiler {compiler} failed: {message}")]
    Compile { compiler: String, message: String },
    #[error("no grammar sources (src/parser.c) found in {0}")]
    NoParser(PathBuf),
    #[error("cannot determine the language's file extensions; pass --extension <EXT>")]
    NoExtensions,
    #[error("grammar `{name}` is already installed at {path} (use --force to overwrite)")]
    AlreadyInstalled { name: String, path: PathBuf },
    #[error("cannot resolve the user-level grammars directory")]
    NoGrammarsDir,
    #[error("building grammar libraries is not supported on this platform")]
    UnsupportedPlatform,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A grammar repository to download and build.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GrammarSource {
    /// Repository clone URL.
    url: String,
    /// Language name: the last path segment of the repository with the
    /// conventional `tree-sitter-` prefix stripped.
    name: String,
}

impl GrammarSource {
    /// Accepts a bare name (`go` -> tree-sitter/tree-sitter-go), an
    /// `org/repo` pair, or a full `http(s)` URL.
    fn parse(source: &str) -> Self {
        let url;
        let repo;
        if let Some(rest) = source
            .strip_prefix("https://")
            .or_else(|| source.strip_prefix("http://"))
        {
            url = source.to_string();
            repo = rest.rsplit('/').next().unwrap_or(rest);
        } else if source.contains('/') {
            url = format!("https://github.com/{source}");
            repo = source.rsplit('/').next().unwrap_or(source);
        } else {
            url = format!("https://github.com/tree-sitter/tree-sitter-{source}");
            repo = source;
        }
        let name = repo.strip_prefix("tree-sitter-").unwrap_or(repo);
        GrammarSource {
            url,
            name: name.to_string(),
        }
    }
}

/// One install invocation.
pub struct InstallRequest {
    /// Bare name, `org/repo`, or repository URL.
    pub source: String,
    /// Extra file extensions (or the only ones, when the grammar has no
    /// `tree-sitter.json` to take them from).
    pub extensions: Vec<String>,
    /// Overwrite an already-installed plugin with the same name.
    pub force: bool,
}

/// A grammar that was installed.
pub struct InstalledGrammar {
    pub name: String,
    pub extensions: Vec<String>,
    pub dir: PathBuf,
}

/// Download, compile and install a grammar plugin.
pub fn install_grammar(request: &InstallRequest) -> Result<InstalledGrammar, InstallError> {
    let source = GrammarSource::parse(&request.source);
    let grammars_dir = user_grammars_dir().ok_or(InstallError::NoGrammarsDir)?;
    let dest = grammars_dir.join(format!("tree-sitter-{}", source.name));
    if dest.exists() && !request.force {
        return Err(InstallError::AlreadyInstalled {
            name: source.name.clone(),
            path: dest,
        });
    }
    let checkout = download(&source)?;
    let mut extensions: Vec<String> = request.extensions.clone();
    if extensions.is_empty() {
        extensions = file_types_from_metadata(&checkout);
    }
    if extensions.is_empty() {
        return Err(InstallError::NoExtensions);
    }
    let sources = grammar_sources(&checkout)?;
    std::fs::create_dir_all(&dest)?;
    let library = format!("libtree-sitter-{}.{}", source.name, shared_lib_extension());
    compile_shared_library(&sources, &dest.join(&library))?;
    write_manifest(
        &dest.join(MANIFEST_FILE_NAME),
        &source.name,
        &extensions,
        &library,
    )?;
    Ok(InstalledGrammar {
        name: source.name,
        extensions,
        dir: dest,
    })
}

/// Shared library file extension for this platform.
pub fn shared_lib_extension() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    }
}

/// Compile tree-sitter C sources into a shared library at `out` with the
/// platform C compiler (overridable via the `CC` environment variable).
pub fn compile_shared_library(sources: &[PathBuf], out: &Path) -> Result<(), InstallError> {
    if !cfg!(unix) {
        return Err(InstallError::UnsupportedPlatform);
    }
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let include = sources
        .first()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let status = Command::new(&compiler)
        .arg("-shared")
        .arg("-fPIC")
        .arg("-I")
        .arg(&include)
        .arg("-o")
        .arg(out)
        .args(sources)
        .status()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                InstallError::CompilerMissing
            } else {
                InstallError::Io(error)
            }
        })?;
    if !status.success() {
        return Err(InstallError::Compile {
            compiler,
            message: format!("exited with {status}"),
        });
    }
    Ok(())
}

fn download(source: &GrammarSource) -> Result<PathBuf, InstallError> {
    let base = cache::user_cache_root().ok_or(InstallError::NoGrammarsDir)?;
    let checkout = base.join("grammar-src").join(&source.name);
    std::fs::remove_dir_all(&checkout).ok();
    std::fs::create_dir_all(checkout.parent().expect("checkout has a parent"))?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", &source.url])
        .arg(&checkout)
        .status()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                InstallError::GitMissing
            } else {
                InstallError::Io(error)
            }
        })?;
    if !status.success() {
        return Err(InstallError::Download {
            url: source.url.clone(),
            message: format!("git clone exited with {status}"),
        });
    }
    Ok(checkout)
}

/// C sources that make up the grammar: `src/parser.c` plus `src/scanner.c`
/// when the grammar ships an external scanner.
fn grammar_sources(checkout: &Path) -> Result<Vec<PathBuf>, InstallError> {
    let src = checkout.join("src");
    let parser = src.join("parser.c");
    if !parser.is_file() {
        return Err(InstallError::NoParser(checkout.to_path_buf()));
    }
    let mut sources = vec![parser];
    let scanner = src.join("scanner.c");
    if scanner.is_file() {
        sources.push(scanner);
    }
    Ok(sources)
}

/// File extensions declared by the grammar's `tree-sitter.json` metadata.
fn file_types_from_metadata(checkout: &Path) -> Vec<String> {
    let Some(text) = std::fs::read_to_string(checkout.join("tree-sitter.json")).ok() else {
        return Vec::new();
    };
    let Ok(metadata) = serde_json::from_str::<TreeSitterJson>(&text) else {
        return Vec::new();
    };
    metadata
        .grammars
        .into_iter()
        .flat_map(|grammar| grammar.file_types)
        .collect()
}

#[derive(Debug, Deserialize)]
struct TreeSitterJson {
    grammars: Vec<GrammarMetadata>,
}

#[derive(Debug, Deserialize)]
struct GrammarMetadata {
    #[serde(rename = "file-types", default)]
    file_types: Vec<String>,
}

fn write_manifest(path: &Path, name: &str, extensions: &[String], library: &str) -> io::Result<()> {
    let extensions = extensions
        .iter()
        .map(|ext| format!("\"{ext}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        path,
        format!("name = \"{name}\"\nextensions = [{extensions}]\nlibrary = \"{library}\"\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_name() {
        let source = GrammarSource::parse("go");
        assert_eq!(source.name, "go");
        assert_eq!(source.url, "https://github.com/tree-sitter/tree-sitter-go");
    }

    #[test]
    fn parses_org_repo() {
        let source = GrammarSource::parse("tree-sitter/tree-sitter-java");
        assert_eq!(source.name, "java");
        assert_eq!(
            source.url,
            "https://github.com/tree-sitter/tree-sitter-java"
        );
    }

    #[test]
    fn parses_full_url() {
        let source = GrammarSource::parse("https://example.com/user/tree-sitter-zig.git");
        assert_eq!(source.name, "zig.git");
        assert_eq!(source.url, "https://example.com/user/tree-sitter-zig.git");
    }

    #[test]
    fn file_types_come_from_tree_sitter_json() {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-install-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tree-sitter.json"),
            r#"{"grammars": [{"name": "go", "file-types": ["go"]}]}"#,
        )
        .unwrap();
        assert_eq!(file_types_from_metadata(&dir), vec!["go".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_lists_extensions_and_library() {
        let dir = std::env::temp_dir().join(format!(
            "dup-detector-install-manifest-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE_NAME);
        write_manifest(
            &path,
            "go",
            &["go".to_string(), "golang".to_string()],
            "libtree-sitter-go.so",
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("name = \"go\""));
        assert!(text.contains("extensions = [\"go\", \"golang\"]"));
        assert!(text.contains("library = \"libtree-sitter-go.so\""));
        std::fs::remove_dir_all(&dir).ok();
    }
}
