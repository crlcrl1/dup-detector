use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Json, tool, tool_router};
use serde::{Deserialize, Serialize};

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
    config_path: Option<PathBuf>,
    config_mtime: Option<SystemTime>,
    last_used: Instant,
}

struct ServerState {
    default_config: Config,
    indexes: Mutex<HashMap<PathBuf, CachedIndex>>,
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

/// Rebuilds the index when the project config file appears, disappears, or
/// changes, so a long-running server picks up `dup-detector.toml` edits.
fn reload_config(state: &ServerState, root: &Path, cached: &mut CachedIndex) -> Result<(), String> {
    let source = Config::source_path(root);
    let mtime = source.as_deref().and_then(file_mtime);
    if source == cached.config_path && mtime == cached.config_mtime {
        return Ok(());
    }
    let config = match &source {
        Some(path) => Config::load_from_file(path).map_err(|error| error.to_string())?,
        None => state.default_config.clone(),
    };
    let mut index = cached
        .index
        .write()
        .map_err(|_| "index lock poisoned".to_string())?;
    *index = SourceIndex::build(root, &config).map_err(|e| format!("failed to index: {e}"))?;
    drop(index);
    cached.config_path = source;
    cached.config_mtime = mtime;
    tracing::info!(root = %root.display(), "configuration reloaded");
    Ok(())
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

impl CloneServer {
    pub fn new(default_config: Config) -> Self {
        Self {
            state: Arc::new(ServerState {
                default_config,
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
        tokio::task::spawn_blocking(move || {
            let root = Path::new(&scope)
                .canonicalize()
                .map_err(|e| format!("cannot resolve scope `{scope}`: {e}"))?;
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
                        reload_config(&state, &root, cached)?;
                        cached.index.clone()
                    }
                    Entry::Vacant(entry) => {
                        let (config, config_path) =
                            match Config::load_with_source(&root).map_err(|e| e.to_string())? {
                                Some((config, path)) => (config, Some(path)),
                                None => (state.default_config.clone(), None),
                            };
                        let index = SourceIndex::build(&root, &config)
                            .map_err(|e| format!("failed to index: {e}"))?;
                        let config_mtime = config_path.as_deref().and_then(file_mtime);
                        entry
                            .insert(CachedIndex {
                                index: Arc::new(RwLock::new(index)),
                                config_path,
                                config_mtime,
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
        })
        .await
        .map_err(|e| format!("scan task panicked: {e}"))?
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesParams {
    /// Subdirectory to scan; defaults to the current working directory.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group (default 7).
    pub min_lines: Option<usize>,
    /// Minimum number of occurrences per group (default 2).
    pub min_occurrences: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
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
    /// Subdirectory to scan; defaults to the current working directory.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group (default 7).
    pub min_lines: Option<usize>,
    /// Minimum number of occurrences per group (default 2).
    pub min_occurrences: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
    pub max_groups: Option<usize>,
    /// Restrict results to these clone types: "type-1", "type-2".
    pub types: Option<Vec<CloneType>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesForRegionParams {
    /// Path of the file containing the region, relative to the scope or absolute.
    pub file: String,
    /// First line of the region (1-based, inclusive).
    pub start_line: u32,
    /// Last line of the region (1-based, inclusive).
    pub end_line: u32,
    /// Subdirectory to scan; defaults to the current working directory.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group; defaults to min(config.min_lines, line span of the region).
    pub min_lines: Option<usize>,
    /// Maximum number of groups to return (default: no limit).
    pub max_groups: Option<usize>,
    /// Restrict results to these clone types: "type-1", "type-2".
    pub types: Option<Vec<CloneType>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReindexParams {
    /// Path whose index should be rebuilt; defaults to the current working directory.
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScanResponse {
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
    pub token_count: usize,
    pub clone_type: CloneType,
    pub occurrences: Vec<OccurrenceDto>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OccurrenceDto {
    pub path: String,
    pub start_line: u32,
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
            .with_index(
                scope.as_deref(),
                move |index, config| -> Result<ScanResponse, String> {
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
                },
            )
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
            .with_index(
                scope.as_deref(),
                move |index, config| -> Result<ScanResponse, String> {
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
                    let allowed =
                        crate::detect::span_window_signatures(file, 0, file.tokens.len(), &cfg);
                    let files: Vec<&SourceFile> = index.files().iter().collect();
                    let groups: Vec<CloneGroup> =
                        crate::detect::detect_filtered(&files, &cfg, Some(&allowed))
                            .into_iter()
                            .filter(|g| g.occurrences.iter().any(|o| o.file as usize == file_index))
                            .collect();
                    Ok(ScanResponse::from_groups(index, groups))
                },
            )
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
            .with_index(
                scope.as_deref(),
                move |index, config| -> Result<ScanResponse, String> {
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
                    let allowed =
                        crate::detect::span_window_signatures(file, span_start, span_end, &cfg);
                    let groups: Vec<CloneGroup> =
                        crate::detect::detect_filtered(&files, &cfg, Some(&allowed))
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
                },
            )
            .await?;
        Ok(Json(response?))
    }

    #[tool(
        description = "Rebuild the in-memory index for `path` (defaults to the current working directory) and drop its on-disk token cache. The next scan re-indexes that root from scratch."
    )]
    async fn reindex(
        &self,
        Parameters(params): Parameters<ReindexParams>,
    ) -> Result<Json<ReindexResponse>, String> {
        let scope = params.path.unwrap_or_else(|| ".".to_string());
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let root = Path::new(&scope)
                .canonicalize()
                .map_err(|e| format!("cannot resolve `{scope}`: {e}"))?;
            let mut indexes = state
                .indexes
                .lock()
                .map_err(|_| "index lock poisoned".to_string())?;
            if let Some(cached) = indexes.remove(&root) {
                // Wait for in-flight detections on the dropped index, otherwise
                // they could write cache entries after the directory is cleared.
                drop(cached.index.write());
            }
            SourceIndex::clear_cache(&root);
            Ok(Json(ReindexResponse { ok: true }))
        })
        .await
        .map_err(|e| format!("reindex task panicked: {e}"))?
    }
}
