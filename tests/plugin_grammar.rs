//! End-to-end tests for the language plugin loader: a real tree-sitter
//! grammar (Go, compiled into a shared library on first use below) is loaded
//! through the plugin manifest flow, and the full detection pipeline runs on
//! it. Compilation happens only when these tests run — plain `cargo build`
//! never touches it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use dup_detector::config::Config;
use dup_detector::index::SourceIndex;
use dup_detector::language::{LanguageId, LanguageRegistry, LanguageSource, registry};
use dup_detector::model::CloneType;
use dup_detector::plugin::{MANIFEST_FILE_NAME, load_plugins_from};

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("dup-detector-plugin-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Shared library built from the tree-sitter-go dev-dependency, compiled on
/// first use into `CARGO_TARGET_TMPDIR` (only set for integration tests).
/// `None` when the toolchain or grammar sources are unavailable, in which
/// case the tests that need it skip.
fn test_grammar_library() -> Option<PathBuf> {
    static LIBRARY: OnceLock<Option<PathBuf>> = OnceLock::new();
    LIBRARY.get_or_init(build_test_grammar).clone()
}

fn build_test_grammar() -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    let source_dir = dep_manifest("tree-sitter-go")?.parent()?.join("src");
    let mut sources: Vec<PathBuf> = ["parser.c", "scanner.c"]
        .iter()
        .map(|name| source_dir.join(name))
        .filter(|path| path.is_file())
        .collect();
    if sources.is_empty() {
        sources.push(source_dir.join("parser.c"));
    }
    if !sources.iter().all(|path| path.is_file()) {
        return None;
    }
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "libtree-sitter-go.{}",
        dup_detector::install::shared_lib_extension()
    ));
    if out.is_file() {
        return Some(out); // reused across test runs
    }
    match dup_detector::install::compile_shared_library(&sources, &out) {
        Ok(()) if out.is_file() => Some(out),
        _ => None,
    }
}

/// Locate a dependency's manifest via `cargo metadata`.
fn dep_manifest(name: &str) -> Option<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    json["packages"]
        .as_array()?
        .iter()
        .find(|package| package["name"].as_str() == Some(name))
        .and_then(|package| package["manifest_path"].as_str())
        .map(PathBuf::from)
}

fn write_plugin_dir(root: &Path, manifest: &str, library: Option<&Path>) {
    let plugin_dir = root.join("tree-sitter-go");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join(MANIFEST_FILE_NAME), manifest).unwrap();
    if let Some(library) = library {
        let target = plugin_dir.join(format!(
            "libtree-sitter-go.{}",
            dup_detector::install::shared_lib_extension()
        ));
        fs::copy(library, target).unwrap();
    }
}

#[test]
fn plugin_grammar_detects_type2_clones() {
    let Some(library) = test_grammar_library() else {
        eprintln!("test grammar library unavailable; skipping plugin e2e test");
        return;
    };
    let grammars = temp_dir("grammars");
    write_plugin_dir(
        &grammars,
        r#"name = "go"
extensions = ["go"]
"#,
        Some(&library),
    );
    let loaded = {
        let mut registry = registry().write().unwrap();
        load_plugins_from(&grammars, &mut registry)
    };
    assert_eq!(loaded.len(), 1, "plugin loads");
    assert_eq!(loaded[0].name, "go");

    let go = LanguageId::from_name("go").expect("go registered");
    assert_eq!(LanguageId::from_extension("go"), Some(go));
    {
        let registry = registry().read().unwrap();
        let source = registry.def(go).unwrap().source();
        assert!(
            matches!(source, LanguageSource::Plugin { .. }),
            "plugin source"
        );
    }

    let project = temp_dir("project");
    let a = r#"package main

func accumulate(values []int) int {
    total := 0
    for _, value := range values {
        if value > 0 {
            total += value
        }
    }
    return total
}
"#;
    let b = a
        .replace("accumulate", "summarize")
        .replace("total", "sum")
        .replace("value", "entry");
    fs::write(project.join("a.go"), a).unwrap();
    fs::write(project.join("b.go"), b).unwrap();
    let config = Config {
        min_lines: 4,
        languages: vec![go],
        ..Config::default()
    };

    let find_pair = || {
        let index = SourceIndex::build_with_cache(&project, &config, None).expect("index builds");
        let groups = index.find_clones(&config);
        let hits = |name: &str| {
            groups.iter().any(|group| {
                group.clone_type == CloneType::Type2
                    && group.occurrences.iter().any(|occurrence| {
                        index.files()[occurrence.file as usize].path.ends_with(name)
                    })
            })
        };
        hits("a.go") && hits("b.go")
    };
    assert!(find_pair(), "first scan finds the Type-2 clone pair");
    assert!(
        find_pair(),
        "second scan reuses the cache entry (grammar fingerprint round-trips)"
    );
    fs::remove_dir_all(&project).ok();
    fs::remove_dir_all(&grammars).ok();
}

#[test]
fn broken_plugin_directories_are_skipped() {
    let root = temp_dir("broken");
    // Missing `extensions`.
    write_plugin_dir(
        &root,
        r#"name = "go"
"#,
        None,
    );
    // Unparseable manifest.
    let bad = root.join("bad-manifest");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join(MANIFEST_FILE_NAME), "not = [toml").unwrap();
    // Manifest with a `library` pointing at a missing file.
    let missing = root.join("missing-library");
    fs::create_dir_all(&missing).unwrap();
    fs::write(
        missing.join(MANIFEST_FILE_NAME),
        r#"name = "go"
extensions = ["go"]
library = "nope.so"
"#,
    )
    .unwrap();

    let mut registry = LanguageRegistry::default();
    let loaded = load_plugins_from(&root, &mut registry);
    assert!(loaded.is_empty(), "no plugin loads, got {loaded:?}");
    assert_eq!(registry.iter().count(), 0, "registry stays empty");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn duplicate_plugin_name_is_rejected() {
    let Some(library) = test_grammar_library() else {
        eprintln!("test grammar library unavailable; skipping duplicate test");
        return;
    };
    let root = temp_dir("duplicate");
    write_plugin_dir(
        &root,
        r#"name = "go"
extensions = ["go"]
"#,
        Some(&library),
    );
    let mut registry = LanguageRegistry::default();
    assert_eq!(load_plugins_from(&root, &mut registry).len(), 1);
    // A second load hits the duplicate-name check and skips the plugin.
    assert!(load_plugins_from(&root, &mut registry).is_empty());
    fs::remove_dir_all(&root).ok();
}
