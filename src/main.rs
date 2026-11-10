use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use dup_detector::config::Config;
use dup_detector::detect::{detect, detect_in_spans};
use dup_detector::index::{SourceIndex, decode_source, parse_file};
use dup_detector::language::{LanguageId, LanguageSource, registry};
use dup_detector::mcp::{CloneServer, ScanResponse};
use dup_detector::model::{CloneGroup, SourceFile};
use dup_detector::tokenize::tokenize;
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "dup-detector",
    version,
    about = "Detect duplicated code (Type-1/Type-2 clones) in a codebase"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run as a long-running MCP server over stdio
    Mcp,
    /// Run as a language server over stdio (diagnostics + jump to duplicates)
    Lsp,
    /// List registered languages (built-in and plugins)
    Languages,
    /// Download a tree-sitter grammar, compile it and install it as a plugin
    Install {
        /// Grammar name (`go`), `org/repo`, or repository URL
        source: String,
        /// File extension for the language; auto-detected from tree-sitter.json when omitted
        #[arg(short = 'e', long = "extension", value_name = "EXT")]
        extensions: Vec<String>,
        /// Overwrite an already-installed plugin with the same name
        #[arg(long)]
        force: bool,
    },
    /// Check the plugin toolchain (git, C compiler, grammars directory, installed plugins)
    Doctor,
    /// Remove the on-disk token cache for a project
    Clean {
        /// Project path whose cache should be removed (defaults to the current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Scan a path and print duplicated code groups
    Scan {
        /// Path to scan (defaults to the current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Minimum number of lines for a clone group (default 7)
        #[arg(long)]
        min_lines: Option<usize>,
        /// Minimum number of occurrences per group (default 2)
        #[arg(long)]
        min_occurrences: Option<usize>,
        /// Maximum number of groups to report (default: no limit)
        #[arg(long)]
        max_groups: Option<usize>,
        /// Allow consistent literal renames to match as clones
        #[arg(long)]
        parameterize_literals: bool,
        /// Restrict scanning to these languages (e.g. --lang rust --lang cpp)
        #[arg(long = "lang", value_name = "LANG")]
        languages: Vec<String>,
        /// Also scan files excluded by .gitignore and .ignore rules
        #[arg(long)]
        no_ignore: bool,
        /// Skip files larger than this many bytes (default 2 MiB)
        #[arg(long)]
        max_file_bytes: Option<u64>,
        /// Print results as JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        targets: Targets,
    },
}

#[derive(clap::Args)]
struct Targets {
    #[arg(
        long = "file",
        value_name = "FILE",
        help = "Only report clone groups that involve this file (repeatable)"
    )]
    files: Vec<PathBuf>,
    #[arg(
        long,
        requires = "files",
        conflicts_with = "stdin_json",
        help = "Read the contents of the single --file from stdin instead of disk"
    )]
    stdin: bool,
    #[arg(
        long,
        help = "Read {\"files\": [{\"path\", \"text\"}], \"exclude\": [path]} from stdin: check those contents in place of the files on disk and leave the excluded files out"
    )]
    stdin_json: bool,
}

#[derive(Deserialize)]
struct Batch {
    files: Vec<BatchFile>,
    #[serde(default)]
    exclude: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct BatchFile {
    path: PathBuf,
    text: String,
}

struct Checked {
    overlays: Vec<SourceFile>,
    on_disk: Vec<PathBuf>,
    exclude: HashSet<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    dup_detector::plugin::load_user_plugins();

    match Cli::parse().command {
        Command::Mcp => run_mcp(),
        Command::Lsp => run_lsp(),
        Command::Languages => {
            print_languages();
            Ok(())
        }
        Command::Install {
            source,
            extensions,
            force,
        } => run_install(&source, extensions, force),
        Command::Doctor => {
            if dup_detector::doctor::run() {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Command::Clean { path } => run_clean(&path),
        Command::Scan {
            path,
            min_lines,
            min_occurrences,
            max_groups,
            parameterize_literals,
            languages,
            no_ignore,
            max_file_bytes,
            json,
            targets,
        } => run_scan(
            &path,
            min_lines,
            min_occurrences,
            max_groups,
            parameterize_literals,
            &languages,
            no_ignore,
            max_file_bytes,
            json,
            &targets,
        ),
    }
}

fn run_mcp() -> anyhow::Result<()> {
    let root = std::env::current_dir().context("cannot resolve the current directory")?;
    let config = Config::load_or_default(&root)
        .with_context(|| format!("failed to load config from {}", root.display()))?;
    let server = CloneServer::new(config, root);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let running = rmcp::serve_server(server, rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    })
}

fn run_lsp() -> anyhow::Result<()> {
    let root = std::env::current_dir().context("cannot resolve the current directory")?;
    let config = Config::load_or_default(&root)
        .with_context(|| format!("failed to load config from {}", root.display()))?;
    dup_detector::lsp::run(config, root)
}

fn run_clean(path: &Path) -> anyhow::Result<()> {
    let root =
        dunce::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    if !root.is_dir() {
        anyhow::bail!("{} is not a directory", root.display());
    }
    let cleared = SourceIndex::clear_cache(&root);
    if cleared.is_empty() {
        println!("No cache found for {}", root.display());
    } else {
        for dir in cleared {
            println!("Removed {}", dir.display());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_scan(
    path: &PathBuf,
    min_lines: Option<usize>,
    min_occurrences: Option<usize>,
    max_groups: Option<usize>,
    parameterize_literals: bool,
    languages: &[String],
    no_ignore: bool,
    max_file_bytes: Option<u64>,
    json: bool,
    targets: &Targets,
) -> anyhow::Result<()> {
    let mut config = Config::load_or_default(path)
        .with_context(|| format!("failed to load config for {}", path.display()))?
        .with_limits(min_lines, min_occurrences, max_groups)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if parameterize_literals {
        config.parameterize_literals = true;
    }
    if no_ignore {
        config.no_ignore = true;
    }
    if let Some(value) = max_file_bytes {
        config = config
            .with_max_file_bytes(value)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if !languages.is_empty() {
        let ids: Vec<LanguageId> = languages
            .iter()
            .map(|name| {
                LanguageId::from_name(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown language: {name}"))
            })
            .collect::<Result<_, _>>()?;
        config.languages = Some(ids);
    }
    let index = SourceIndex::build(path, &config)
        .with_context(|| format!("failed to index {}", path.display()))?;
    let checked = checked_targets(&index, targets, &config)?;
    let (files, positions) = index.files_with(&checked.overlays, &checked.exclude);
    let mut spans: Vec<(usize, usize, usize)> = positions
        .iter()
        .map(|&position| (position, 0, files[position].tokens.len()))
        .collect();
    for target in &checked.on_disk {
        let position = files
            .iter()
            .position(|file| file.path == *target)
            .ok_or_else(|| anyhow::anyhow!("{} is excluded", target.display()))?;
        spans.push((position, 0, files[position].tokens.len()));
    }
    let targeted = !targets.files.is_empty() || targets.stdin_json;
    let groups = match (targeted, spans.is_empty()) {
        (false, _) => detect(&files, &config),
        (true, true) => Vec::new(),
        (true, false) => detect_in_spans(&files, &spans, &config),
    };
    if json {
        let mut out = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut out, &ScanResponse::from_files(&files, groups))?;
        writeln!(out)?;
    } else {
        println!(
            "{} clone group(s) across {} file(s):",
            groups.len(),
            files.len()
        );
        for (n, group) in groups.iter().enumerate() {
            print_group(n + 1, group, &files);
        }
    }
    std::mem::forget(index);
    Ok(())
}

fn resolve_target(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut missing = Vec::new();
    let mut existing = absolute.as_path();
    loop {
        match dunce::canonicalize(existing) {
            Ok(found) => return Ok(missing.iter().rev().fold(found, |acc, part| acc.join(part))),
            Err(error) => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(error.into());
                };
                missing.push(name);
                existing = parent;
            }
        }
    }
}

fn checked_targets(
    index: &SourceIndex,
    targets: &Targets,
    config: &Config,
) -> anyhow::Result<Checked> {
    let mut checked = Checked {
        overlays: Vec::new(),
        on_disk: Vec::new(),
        exclude: HashSet::new(),
    };
    if targets.stdin_json {
        let batch: Batch = serde_json::from_reader(std::io::stdin().lock())
            .context("stdin is not a valid --stdin-json batch")?;
        let mut latest: Vec<(PathBuf, String)> = Vec::new();
        for file in batch.files {
            let path = resolve_target(&file.path)?;
            latest.retain(|(existing, _)| *existing != path);
            latest.push((path, file.text));
        }
        for (path, text) in latest {
            overlay(path, text, config, &mut checked)?;
        }
        for path in batch.exclude {
            checked.exclude.insert(resolve_target(&path)?);
        }
    }
    if targets.stdin {
        let [path] = targets.files.as_slice() else {
            anyhow::bail!("--stdin reads the contents of exactly one --file");
        };
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        overlay(
            resolve_target(path)?,
            decode_source(bytes),
            config,
            &mut checked,
        )?;
        return Ok(checked);
    }
    for path in &targets.files {
        let path = resolve_target(path)?;
        if checked.exclude.contains(&path) || checked.overlays.iter().any(|file| file.path == path)
        {
            continue;
        }
        if index.file_by_path(&path).is_some() {
            checked.on_disk.push(path);
            continue;
        }
        let language = language_of(&path, config)?;
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > config.max_file_bytes) {
            continue;
        }
        let parsed = parse_file(&path, language, config.max_file_bytes)
            .ok_or_else(|| anyhow::anyhow!("cannot read or parse {}", path.display()))?;
        checked.overlays.push(parsed);
    }
    Ok(checked)
}

fn language_of(path: &Path, config: &Config) -> anyhow::Result<LanguageId> {
    LanguageId::from_path(path)
        .filter(|language| config.language_enabled(*language))
        .ok_or_else(|| anyhow::anyhow!("{} is not a scanned source file", path.display()))
}

fn overlay(
    path: PathBuf,
    text: String,
    config: &Config,
    checked: &mut Checked,
) -> anyhow::Result<()> {
    let language = language_of(&path, config)?;
    if text.len() as u64 > config.max_file_bytes {
        checked.exclude.insert(path);
        return Ok(());
    }
    let tokens = tokenize(&text, language)?;
    let size = text.len() as u64;
    checked
        .overlays
        .push(SourceFile::new(path, language, text, tokens, None, size));
    Ok(())
}

fn run_install(source: &str, extensions: Vec<String>, force: bool) -> anyhow::Result<()> {
    let installed =
        dup_detector::install::install_grammar(&dup_detector::install::InstallRequest {
            source: source.to_string(),
            extensions,
            force,
        })
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let shown = installed
        .extensions
        .iter()
        .map(|ext| format!(".{ext}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "installed {} ({}) -> {}",
        installed.name,
        shown,
        installed.dir.display()
    );
    println!(
        "run `dup-detector languages` to verify, then scan with --lang {}",
        installed.name
    );
    Ok(())
}

/// Maximum width of the extensions column; longer lists are truncated with
/// `…` (the full list lives in the plugin's `grammar.toml`).
const EXTENSIONS_COLUMN_MAX: usize = 32;

fn print_languages() {
    struct Row {
        name: String,
        extensions: String,
        abi: String,
        source: String,
    }
    let registry = registry().read().expect("language registry poisoned");
    let mut rows = Vec::new();
    for (_, def) in registry.iter() {
        let source = match def.source() {
            LanguageSource::Builtin => "built-in".to_string(),
            LanguageSource::Plugin { dir } => format!(
                "plugin ({})",
                dir.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?")
            ),
        };
        rows.push(Row {
            name: def.name().to_string(),
            extensions: format_extensions(def.extensions()),
            abi: def.grammar().abi_version().to_string(),
            source,
        });
    }
    let name_width = rows
        .iter()
        .map(|row| row.name.chars().count())
        .max()
        .unwrap_or(0)
        .max("Language".len());
    let ext_width = rows
        .iter()
        .map(|row| row.extensions.chars().count())
        .max()
        .unwrap_or(0)
        .max("Extensions".len());
    println!(
        "{:<nw$}  {:<ew$}  {:>3}  Source",
        "Language",
        "Extensions",
        "ABI",
        nw = name_width,
        ew = ext_width,
    );
    for row in &rows {
        println!(
            "{:<nw$}  {:<ew$}  {:>3}  {}",
            row.name,
            row.extensions,
            row.abi,
            row.source,
            nw = name_width,
            ew = ext_width,
        );
    }
}

fn format_extensions(extensions: &[String]) -> String {
    let joined = extensions
        .iter()
        .map(|ext| format!(".{ext}"))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.chars().count() <= EXTENSIONS_COLUMN_MAX {
        return joined;
    }
    let mut shown = String::new();
    for ext in extensions {
        let candidate = if shown.is_empty() {
            format!(".{ext}")
        } else {
            format!("{shown} .{ext}")
        };
        if candidate.chars().count() + 2 > EXTENSIONS_COLUMN_MAX {
            break;
        }
        shown = candidate;
    }
    format!("{shown} …")
}

fn print_group(n: usize, group: &CloneGroup, files: &[&SourceFile]) {
    println!(
        "#{n}: {} tokens, {:?}, {} occurrence(s)",
        group.token_count,
        group.clone_type,
        group.occurrences.len()
    );
    for occ in &group.occurrences {
        let file = files[occ.file as usize];
        let start_line = file.token_line(occ.start as usize);
        let end_line = file.token_end_line((occ.end - 1) as usize);
        println!("  {}:{start_line}:{end_line}", file.path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_target_appends_missing_components_to_existing_ancestor() {
        let dir = std::env::temp_dir().join(format!("dup-detector-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = resolve_target(&dir.join("new").join("file.rs")).unwrap();
        assert_eq!(
            resolved,
            dunce::canonicalize(&dir)
                .unwrap()
                .join("new")
                .join("file.rs")
        );
        assert_eq!(
            resolve_target(&dir).unwrap(),
            dunce::canonicalize(&dir).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
