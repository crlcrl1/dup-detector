use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

struct ServerState {
    config: Config,
    indexes: Mutex<HashMap<PathBuf, SourceIndex>>,
}

impl CloneServer {
    pub fn new(config: Config) -> Self {
        Self {
            state: Arc::new(ServerState {
                config,
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
            let mut indexes = state
                .indexes
                .lock()
                .map_err(|_| "index lock poisoned".to_string())?;
            let index = match indexes.entry(root) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let key = entry.key().clone();
                    entry.insert(
                        SourceIndex::build(&key, &state.config)
                            .map_err(|e| format!("failed to index: {e}"))?,
                    )
                }
            };
            index.refresh();
            Ok(f(index, &state.config))
        })
        .await
        .map_err(|e| format!("scan task panicked: {e}"))?
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClonesParams {
    /// Subdirectory to scan; defaults to the current working directory.
    pub scope: Option<String>,
    /// Minimum number of lines for a clone group (default 5).
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
    /// Minimum number of lines for a clone group (default 5).
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
    /// Minimum number of lines for a clone group; defaults to the line span of the region.
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
            let start_line = file.tokens[occ.start as usize].line;
            let end_line = file.tokens[(occ.end - 1) as usize].end_line;
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
    absolute.canonicalize().ok()
}

fn token_span_for_lines(
    file: &SourceFile,
    start_line: u32,
    end_line: u32,
) -> Option<(usize, usize)> {
    let tokens = &file.tokens;
    let start = tokens.partition_point(|t| t.line < start_line);
    let end = tokens.partition_point(|t| t.line <= end_line);
    (start < end).then_some((start, end))
}

fn line_span(file: &SourceFile, start: usize, end: usize) -> usize {
    let first = file.tokens[start].line as usize;
    let last = file.tokens[end - 1].end_line as usize;
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
                    let mut cfg = config.clone().with_limits(
                        params.min_lines,
                        params.min_occurrences,
                        params.max_groups,
                    );
                    if let Some(v) = params.parameterize_literals {
                        cfg.parameterize_literals = v;
                    }
                    let groups: Vec<CloneGroup> = index
                        .find_clones(&cfg)
                        .into_iter()
                        .filter(|g| {
                            params
                                .types
                                .as_ref()
                                .is_none_or(|types| types.contains(&g.clone_type))
                        })
                        .collect();
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
                    let cfg = config.clone().with_limits(
                        params.min_lines,
                        params.min_occurrences,
                        params.max_groups,
                    );
                    let groups: Vec<CloneGroup> = index
                        .find_clones(&cfg)
                        .into_iter()
                        .filter(|g| {
                            g.occurrences.iter().any(|o| o.file as usize == file_index)
                                && params
                                    .types
                                    .as_ref()
                                    .is_none_or(|types| types.contains(&g.clone_type))
                        })
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
                        parsed = parse_file(&resolved, language)
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
                    let cfg = config
                        .clone()
                        .with_limits(Some(min_lines), None, params.max_groups);
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
                                }) && params
                                    .types
                                    .as_ref()
                                    .is_none_or(|types| types.contains(&g.clone_type))
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
            indexes.remove(&root);
            SourceIndex::clear_cache(&root);
            Ok(Json(ReindexResponse { ok: true }))
        })
        .await
        .map_err(|e| format!("reindex task panicked: {e}"))?
    }
}
