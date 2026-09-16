use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use dup_detector::config::Config;
use dup_detector::index::SourceIndex;
use dup_detector::language::LanguageId;
use dup_detector::model::CloneGroup;
use dup_detector::server::{CloneServer, ScanResponse};
use tracing_subscriber::EnvFilter;

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
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Mcp => run_mcp(),
        Command::Lsp => run_lsp(),
        Command::Scan {
            path,
            min_lines,
            min_occurrences,
            max_groups,
            parameterize_literals,
            languages,
            json,
        } => run_scan(
            &path,
            min_lines,
            min_occurrences,
            max_groups,
            parameterize_literals,
            &languages,
            json,
        ),
    }
}

fn run_mcp() -> anyhow::Result<()> {
    let root = std::env::current_dir().context("cannot resolve the current directory")?;
    let config = Config::load_or_default(&root)
        .with_context(|| format!("failed to load config from {}", root.display()))?;
    let server = CloneServer::new(config);
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

#[allow(clippy::too_many_arguments)]
fn run_scan(
    path: &PathBuf,
    min_lines: Option<usize>,
    min_occurrences: Option<usize>,
    max_groups: Option<usize>,
    parameterize_literals: bool,
    languages: &[String],
    json: bool,
) -> anyhow::Result<()> {
    let mut config = Config::load_or_default(path)
        .with_context(|| format!("failed to load config for {}", path.display()))?
        .with_limits(min_lines, min_occurrences, max_groups);
    if parameterize_literals {
        config.parameterize_literals = true;
    }
    if !languages.is_empty() {
        let ids: Vec<LanguageId> = languages
            .iter()
            .map(|name| {
                LanguageId::from_name(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown language: {name}"))
            })
            .collect::<Result<_, _>>()?;
        config.languages = ids;
    }
    let index = SourceIndex::build(path, &config)
        .with_context(|| format!("failed to index {}", path.display()))?;
    let groups = index.find_clones(&config);
    if json {
        let response = ScanResponse::from_groups(&index, groups);
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!(
            "{} clone group(s) across {} file(s):",
            groups.len(),
            index.files().len()
        );
        for (n, group) in groups.iter().enumerate() {
            print_group(n + 1, group, &index);
        }
    }
    Ok(())
}

fn print_group(n: usize, group: &CloneGroup, index: &SourceIndex) {
    let files = index.files();
    println!(
        "#{n}: {} tokens, {:?}, {} occurrence(s)",
        group.token_count,
        group.clone_type,
        group.occurrences.len()
    );
    for occ in &group.occurrences {
        let file = &files[occ.file as usize];
        let start_line = file.tokens[occ.start as usize].line;
        let end_line = file.tokens[(occ.end - 1) as usize].end_line;
        println!("  {}:{start_line}:{end_line}", file.path.display());
    }
}
