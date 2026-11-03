use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Json, tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::config::Config;
use crate::index::{SourceIndex, parse_file};
use crate::language::LanguageId;
use crate::model::{CloneGroup, CloneType, SourceFile};

#[derive(Clone)]
pub struct CloneServer {
    state: Arc<ServerState>,
}

const MAX_CACHED_INDEXES: usize = 8;

struct CachedIndex {
    index: Arc<RwLock<SourceIndex>>,
    config_sources: Vec<(PathBuf, Option<SystemTime>)>,
    last_used: Instant,
}

struct ServerState {
    default_config: Config,
    /// Canonicalized startup directory; scopes must stay inside it and the
    /// on-disk cache lives at its `.dup-detector/` regardless of scope.
    workspace_root: PathBuf,
    indexes: Mutex<HashMap<PathBuf, CachedIndex>>,
}

/// Rebuilds the index when a config file (user-level or project-level) appears,
/// disappears, or changes, so a long-running server picks up config edits.
fn reload_config(state: &ServerState, root: &Path, cached: &mut CachedIndex) -> Result<(), String> {
    let sources = Config::source_stamps(root);
    if sources == cached.config_sources {
        return Ok(());
    }
    let config = match Config::load(root).map_err(|error| error.to_string())? {
        Some(config) => config,
        None => state.default_config.clone(),
    };
    let mut index = cached
        .index
        .write()
        .map_err(|_| "index lock poisoned".to_string())?;
    *index = build_index(state, root, &config)?;
    drop(index);
    cached.config_sources = sources;
    tracing::info!(root = %root.display(), "configuration reloaded");
    Ok(())
}

/// Builds the index for `root`, keeping the on-disk cache at the workspace
/// root — keyed by workspace-relative paths — so scopes never create their own
/// `.dup-detector/` directories and share one cache entry per file.
fn build_index(state: &ServerState, root: &Path, config: &Config) -> Result<SourceIndex, String> {
    if root != state.workspace_root {
        // Versions that cached inside the scope directory left a stray
        // `.dup-detector/` there; remove it once.
        let _ = cache::clear(&cache::dir_in(root));
    }
    let cache_dir = cache::dir_for(&state.workspace_root, config.cache_location);
    SourceIndex::build_with_cache_base(root, config, cache_dir.as_deref(), &state.workspace_root)
        .map_err(|e| format!("failed to index: {e}"))
}

fn evict_least_recently_used(indexes: &mut HashMap<PathBuf, CachedIndex>) {
    let oldest = indexes
        .iter()
        .min_by_key(|(_, cached)| cached.last_used)
        .map(|(path, _)| path.clone());
    if let Some(path) = oldest {
        indexes.remove(&path);
    }
}

fn run_with_index<T, F>(state: &ServerState, scope: &str, f: F) -> Result<T, String>
where
    F: FnOnce(&SourceIndex, &Config) -> T,
{
    let root = Path::new(scope)
        .canonicalize()
        .map_err(|e| format!("cannot resolve scope `{scope}`: {e}"))?;
    if !root.starts_with(&state.workspace_root) {
        return Err(format!(
            "scope `{}` is outside the working directory `{}`; only the working directory and its subdirectories can be scanned",
            root.display(),
            state.workspace_root.display()
        ));
    }
    let holder = {
        let mut indexes = state
            .indexes
            .lock()
            .map_err(|_| "index lock poisoned".to_string())?;
        if indexes.len() >= MAX_CACHED_INDEXES && !indexes.contains_key(&root) {
            evict_least_recently_used(&mut indexes);
        }
        match indexes.entry(root.clone()) {
            Entry::Occupied(mut entry) => {
                let cached = entry.get_mut();
                cached.last_used = Instant::now();
                reload_config(state, &root, cached)?;
                cached.index.clone()
            }
            Entry::Vacant(entry) => {
                let config = match Config::load(&root).map_err(|e| e.to_string())? {
                    Some(config) => config,
                    None => state.default_config.clone(),
                };
                let index = build_index(state, &root, &config)?;
                let config_sources = Config::source_stamps(&root);
                entry
                    .insert(CachedIndex {
                        index: Arc::new(RwLock::new(index)),
                        config_sources,
                        last_used: Instant::now(),
                    })
                    .index
                    .clone()
            }
        }
    };
    {
        // Refresh briefly under the write lock; detection below runs
        // under the read lock so concurrent tool calls parallelize.
        let mut index = holder
            .write()
            .map_err(|_| "index lock poisoned".to_string())?;
        index.refresh();
    }
    let index = holder
        .read()
        .map_err(|_| "index lock poisoned".to_string())?;
    let config = index.config().clone();
    Ok(f(&index, &config))
}

impl CloneServer {
    pub fn new(default_config: Config, root: PathBuf) -> Self {
        let workspace_root = root.canonicalize().unwrap_or(root);
        Self {
            state: Arc::new(ServerState {
                default_config,
                workspace_root,
                indexes: Mutex::new(HashMap::new()),
            }),
        }
    }

    async fn with_index<T, F>(&self, scope: Option<&str>, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&SourceIndex, &Config) -> T + Send + 'static,
    {
        let scope = scope.unwrap_or(".").to_string();
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || run_with_index(&state, &scope, f))
            .await
            .map_err(|e| format!("scan task panicked: {e}"))?
    }
}

// schemars tags `usize`/`u32` with non-standard `format` values (`uint`/`uint32`) that
// strict MCP schema validators reject as unknown. These emit a plain integer schema with
// no `format`; they only affect the generated JSON schema, not serde (de)serialization.
fn integer_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "integer"
    })
}

fn nullable_integer_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"]
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesParams {
    /// Subdirectory of the current working directory to scan; defaults to the working directory itself. Paths outside the working directory are rejected.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group (default 7).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub min_lines: Option<usize>,
    /// Minimum number of occurrences per group (default 2).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub min_occurrences: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub max_groups: Option<usize>,
    /// Restrict results to these clone types: "type-1", "type-2".
    pub types: Option<Vec<CloneType>>,
    /// Allow consistent literal renames to match as clones.
    pub parameterize_literals: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesInFileParams {
    /// Path of the file to inspect, relative to the scope or absolute.
    pub file: String,
    /// Subdirectory of the current working directory to scan; defaults to the working directory itself. Paths outside the working directory are rejected.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group (default 7).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub min_lines: Option<usize>,
    /// Minimum number of occurrences per group (default 2).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub min_occurrences: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub max_groups: Option<usize>,
    /// Restrict results to these clone types: "type-1", "type-2".
    pub types: Option<Vec<CloneType>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesForRegionParams {
    /// Path of the file containing the region, relative to the scope or absolute.
    pub file: String,
    /// First line of the region (1-based, inclusive).
    #[schemars(schema_with = "integer_schema")]
    pub start_line: u32,
    /// Last line of the region (1-based, inclusive).
    #[schemars(schema_with = "integer_schema")]
    pub end_line: u32,
    /// Subdirectory of the current working directory to scan; defaults to the working directory itself. Paths outside the working directory are rejected.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group; defaults to min(config.min_lines, line span of the region).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub min_lines: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
    #[schemars(schema_with = "nullable_integer_schema")]
    pub max_groups: Option<usize>,
    /// Restrict results to these clone types: "type-1", "type-2".
    pub types: Option<Vec<CloneType>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScanResponse {
    #[schemars(schema_with = "integer_schema")]
    pub files_scanned: usize,
    pub groups: Vec<CloneGroupDto>,
}

impl ScanResponse {
    pub fn from_groups(index: &SourceIndex, groups: Vec<CloneGroup>) -> Self {
        let files: Vec<&SourceFile> = index.files().iter().collect();
        let groups = groups.into_iter().map(|g| to_dto(&files, &g)).collect();
        Self {
            files_scanned: files.len(),
            groups,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CloneGroupDto {
    #[schemars(schema_with = "integer_schema")]
    pub token_count: usize,
    pub clone_type: CloneType,
    pub occurrences: Vec<OccurrenceDto>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OccurrenceDto {
    pub path: String,
    #[schemars(schema_with = "integer_schema")]
    pub start_line: u32,
    #[schemars(schema_with = "integer_schema")]
    pub end_line: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReindexResponse {
    pub ok: bool,
}

fn to_dto(files: &[&SourceFile], group: &CloneGroup) -> CloneGroupDto {
    let occurrences = group
        .occurrences
        .iter()
        .map(|occ| {
            let file = files[occ.file as usize];
            let start_line = file.token_line(occ.start as usize);
            let end_line = file.token_end_line((occ.end - 1) as usize);
            OccurrenceDto {
                path: file.path.display().to_string(),
                start_line,
                end_line,
            }
        })
        .collect();
    CloneGroupDto {
        token_count: group.token_count,
        clone_type: group.clone_type,
        occurrences,
    }
}

fn resolve_file(root: &Path, file: &str) -> Option<PathBuf> {
    let path = Path::new(file);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let absolute = absolute.canonicalize().ok()?;
    // `root` is canonicalized at index build time; reject paths that escape it
    // (e.g. via `..`), otherwise tools would read files outside the scope.
    absolute.starts_with(root).then_some(absolute)
}

fn token_span_for_lines(
    file: &SourceFile,
    start_line: u32,
    end_line: u32,
) -> Option<(usize, usize)> {
    let tokens = &file.tokens;
    let first = file.line_offset(start_line);
    let limit = if end_line == u32::MAX {
        usize::MAX
    } else {
        file.line_offset(end_line + 1)
    };
    let start = tokens.partition_point(|t| (t.start as usize) < first);
    let end = tokens.partition_point(|t| (t.start as usize) < limit);
    (start < end).then_some((start, end))
}

fn line_span(file: &SourceFile, start: usize, end: usize) -> usize {
    let first = file.token_line(start) as usize;
    let last = file.token_end_line(end - 1) as usize;
    last.saturating_sub(first) + 1
}

fn find_clones_in_index(
    index: &SourceIndex,
    config: &Config,
    params: FindClonesParams,
) -> Result<ScanResponse, String> {
    let mut cfg = config
        .clone()
        .with_limits(params.min_lines, params.min_occurrences, params.max_groups)
        .map_err(|e| e.to_string())?;
    if let Some(v) = params.parameterize_literals {
        cfg.parameterize_literals = v;
    }
    cfg.types = params.types;
    let groups = index.find_clones(&cfg);
    Ok(ScanResponse::from_groups(index, groups))
}

fn find_clones_in_file_from_index(
    index: &SourceIndex,
    config: &Config,
    params: FindClonesInFileParams,
) -> Result<ScanResponse, String> {
    let resolved = resolve_file(index.root(), &params.file).ok_or_else(|| {
        format!(
            "cannot resolve file `{}` under `{}`",
            params.file,
            index.root().display()
        )
    })?;
    let file_index = index.file_by_path(&resolved).ok_or_else(|| {
        format!(
            "`{}` is not a supported source file in the project",
            params.file
        )
    })?;
    let mut cfg = config
        .clone()
        .with_limits(params.min_lines, params.min_occurrences, params.max_groups)
        .map_err(|e| e.to_string())?;
    cfg.types = params.types;
    let file = &index.files()[file_index];
    let allowed = crate::detect::span_window_signatures(file, 0, file.tokens.len(), &cfg);
    let files: Vec<&SourceFile> = index.files().iter().collect();
    let groups: Vec<CloneGroup> = crate::detect::detect_filtered(&files, &cfg, Some(&allowed))
        .into_iter()
        .filter(|g| g.occurrences.iter().any(|o| o.file as usize == file_index))
        .collect();
    Ok(ScanResponse::from_groups(index, groups))
}

fn find_clones_for_region_in_index(
    index: &SourceIndex,
    config: &Config,
    params: FindClonesForRegionParams,
) -> Result<ScanResponse, String> {
    if params.start_line == 0 {
        return Err("`start_line` is 1-based and must be at least 1".to_string());
    }
    if params.end_line < params.start_line {
        return Err(format!(
            "`end_line` ({}) must be >= `start_line` ({})",
            params.end_line, params.start_line
        ));
    }
    let resolved = resolve_file(index.root(), &params.file).ok_or_else(|| {
        format!(
            "cannot resolve file `{}` under `{}`",
            params.file,
            index.root().display()
        )
    })?;
    let parsed;
    let files: Vec<&SourceFile>;
    let (file, file_idx) = if let Some(i) = index.file_by_path(&resolved) {
        files = index.files().iter().collect();
        (files[i], i)
    } else {
        let language = LanguageId::from_path(&resolved)
            .ok_or_else(|| format!("unsupported file type: `{}`", params.file))?;
        if !config.language_enabled(language) {
            return Err(format!(
                "language `{language}` is disabled by configuration"
            ));
        }
        parsed = parse_file(&resolved, language, config.max_file_bytes)
            .ok_or_else(|| format!("cannot read or parse `{}`", params.file))?;
        files = index
            .files()
            .iter()
            .chain(std::iter::once(&parsed))
            .collect();
        let idx = files.len() - 1;
        (files[idx], idx)
    };
    let Some((span_start, span_end)) =
        token_span_for_lines(file, params.start_line, params.end_line)
    else {
        return Ok(ScanResponse {
            files_scanned: files.len(),
            groups: Vec::new(),
        });
    };
    let region_lines = line_span(file, span_start, span_end);
    let min_lines = params
        .min_lines
        .unwrap_or(config.min_lines.min(region_lines));
    let mut cfg = config
        .clone()
        .with_limits(Some(min_lines), None, params.max_groups)
        .map_err(|e| e.to_string())?;
    cfg.types = params.types;
    let allowed = crate::detect::span_window_signatures(file, span_start, span_end, &cfg);
    let groups: Vec<CloneGroup> = crate::detect::detect_filtered(&files, &cfg, Some(&allowed))
        .into_iter()
        .filter(|g| {
            g.occurrences.iter().any(|o| {
                o.file as usize == file_idx
                    && o.start < span_end as u32
                    && o.end > span_start as u32
            })
        })
        .collect();
    Ok(ScanResponse {
        files_scanned: files.len(),
        groups: groups.iter().map(|g| to_dto(&files, g)).collect(),
    })
}

#[tool_router(server_handler)]
impl CloneServer {
    #[tool(
        description = "Find duplicated code across the project (or a subdirectory). Returns clone groups sorted by size; each group lists its file paths, line ranges, token count and clone type. Token-efficient: does not include source excerpts."
    )]
    async fn find_clones(
        &self,
        Parameters(params): Parameters<FindClonesParams>,
    ) -> Result<Json<ScanResponse>, String> {
        let scope = params.scope.clone();
        let response = self
            .with_index(scope.as_deref(), move |index, config| {
                find_clones_in_index(index, config, params)
            })
            .await?;
        Ok(Json(response?))
    }

    #[tool(
        description = "Find clones that involve a specific file. `file` may be relative to the project root or absolute."
    )]
    async fn find_clones_in_file(
        &self,
        Parameters(params): Parameters<FindClonesInFileParams>,
    ) -> Result<Json<ScanResponse>, String> {
        let scope = params.scope.clone();
        let response = self
            .with_index(scope.as_deref(), move |index, config| {
                find_clones_in_file_from_index(index, config, params)
            })
            .await?;
        Ok(Json(response?))
    }

    #[tool(
        description = "Check whether the code in `file` between `start_line` and `end_line` (1-based, inclusive) is duplicated elsewhere in the project. Returns clone groups that contain the region, including their other occurrences. If the file is not part of the index yet (e.g. newly created), it is parsed on the fly."
    )]
    async fn find_clones_for_region(
        &self,
        Parameters(params): Parameters<FindClonesForRegionParams>,
    ) -> Result<Json<ScanResponse>, String> {
        let scope = params.scope.clone();
        let response = self
            .with_index(scope.as_deref(), move |index, config| {
                find_clones_for_region_in_index(index, config, params)
            })
            .await?;
        Ok(Json(response?))
    }

    #[tool(
        description = "Rebuild the in-memory indexes and drop the on-disk token cache of the current working directory. The next scan re-indexes from scratch."
    )]
    async fn reindex(&self) -> Result<Json<ReindexResponse>, String> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut indexes = state
                .indexes
                .lock()
                .map_err(|_| "index lock poisoned".to_string())?;
            for (_, cached) in indexes.drain() {
                // Wait for in-flight detections on the dropped index, otherwise
                // they could write cache entries after the directory is cleared.
                drop(cached.index.write());
            }
            SourceIndex::clear_cache(&state.workspace_root);
            Ok(Json(ReindexResponse { ok: true }))
        })
        .await
        .map_err(|e| format!("reindex task panicked: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dup-detector-server-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_with_workspace(workspace: &Path) -> ServerState {
        ServerState {
            default_config: Config::default(),
            workspace_root: workspace.canonicalize().unwrap(),
            indexes: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn scope_outside_workspace_is_rejected() {
        let workspace = temp_dir("scope-reject-ws");
        let elsewhere = temp_dir("scope-reject-out");
        let state = state_with_workspace(&workspace);
        let result = run_with_index(&state, elsewhere.to_str().unwrap(), |_, _| ());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("outside the working directory")
        );
        fs::remove_dir_all(&workspace).ok();
        fs::remove_dir_all(&elsewhere).ok();
    }

    #[test]
    fn subdir_scope_uses_workspace_cache_dir() {
        let workspace = temp_dir("scope-cache-ws");
        let sub = workspace.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let source = "fn f() {\n    let x = 1;\n    let y = 2;\n    let z = x + y;\n    println!(\"{z}\");\n}\n";
        fs::write(sub.join("a.rs"), source).unwrap();
        fs::write(sub.join("b.rs"), source).unwrap();
        let state = state_with_workspace(&workspace);
        let entry_count = |dir: &Path| {
            fs::read_dir(dir)
                .map(|entries| entries.count())
                .unwrap_or(0)
        };
        // Index the subdir scope first: the cache lands at the workspace root.
        let scope = sub.canonicalize().unwrap();
        let files = run_with_index(&state, scope.to_str().unwrap(), |index, _| {
            index.files().len()
        })
        .unwrap();
        assert_eq!(files, 2);
        assert!(workspace.join(".dup-detector").is_dir());
        assert!(!sub.join(".dup-detector").exists());
        let count = entry_count(&workspace.join(".dup-detector"));
        assert_eq!(count, 2);
        // Indexing the whole workspace afterwards must reuse the same entries.
        let root = workspace.canonicalize().unwrap();
        let files = run_with_index(&state, root.to_str().unwrap(), |index, _| {
            index.files().len()
        })
        .unwrap();
        assert_eq!(files, 2);
        assert_eq!(entry_count(&workspace.join(".dup-detector")), count);
        fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn reindex_clears_workspace_cache_and_drops_indexes() {
        let workspace = temp_dir("reindex-ws");
        fs::write(workspace.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        let state = state_with_workspace(&workspace);
        let root = workspace.canonicalize().unwrap();
        run_with_index(&state, root.to_str().unwrap(), |index, _| {
            assert_eq!(index.files().len(), 1)
        })
        .unwrap();
        assert!(!state.indexes.lock().unwrap().is_empty());
        assert!(workspace.join(".dup-detector").is_dir());
        state.indexes.lock().unwrap().clear();
        SourceIndex::clear_cache(&state.workspace_root);
        assert!(!workspace.join(".dup-detector").exists());
        fs::remove_dir_all(&workspace).ok();
    }
}
