use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, InitializeResult, InitializedParams, Location, MarkupContent, MarkupKind,
    MessageType, OneOf, Position, Range, ReferenceParams, ServerCapabilities, ServerInfo,
    TextDocumentContentChangeEvent, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

use crate::config::Config;
use crate::detect;
use crate::fast_hash::FastSet;
use crate::index::SourceIndex;
use crate::language::LanguageId;
use crate::model::{CloneType, Occurrence, SourceFile};
use crate::tokenize;

const DEBOUNCE: Duration = Duration::from_millis(350);
const MAX_DIAGNOSTICS_PER_FILE: usize = 100;
const SOURCE: &str = "dup-detector";

struct Document {
    uri: Url,
    path: PathBuf,
    file: Arc<SourceFile>,
}

#[derive(Debug, Clone, PartialEq)]
enum ChangedRegion {
    Whole,
    Bytes(Vec<(usize, usize)>),
}

#[derive(Default, Clone)]
struct PendingChanges {
    changed: HashMap<PathBuf, ChangedRegion>,
}

impl PendingChanges {
    fn mark_whole(&mut self, path: PathBuf) {
        self.changed.insert(path, ChangedRegion::Whole);
    }

    fn mark_bytes(&mut self, path: PathBuf, start: usize, end: usize) {
        match self.changed.entry(path) {
            Entry::Occupied(mut entry) => {
                if let ChangedRegion::Bytes(ranges) = entry.get_mut() {
                    ranges.push((start, end));
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(ChangedRegion::Bytes(vec![(start, end)]));
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

struct State {
    root: PathBuf,
    config: Config,
    documents: Mutex<HashMap<PathBuf, Arc<Document>>>,
    update_lock: Mutex<()>,
    index: Mutex<Option<SourceIndex>>,
    snapshot: Mutex<Arc<Snapshot>>,
    pending: Mutex<PendingChanges>,
    generation: AtomicU64,
    notify: Notify,
}

impl State {
    fn new(root: PathBuf, config: Config) -> Self {
        Self {
            root,
            config,
            documents: Mutex::new(HashMap::new()),
            update_lock: Mutex::new(()),
            index: Mutex::new(None),
            snapshot: Mutex::new(Arc::new(Snapshot::default())),
            pending: Mutex::new(PendingChanges::default()),
            generation: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Snapshot {
    groups: Vec<NavGroup>,
    signatures: FastSet<u64>,
}

struct NavGroup {
    token_count: usize,
    clone_type: CloneType,
    occurrences: Vec<NavOccurrence>,
}

struct NavOccurrence {
    path: PathBuf,
    uri: Url,
    range: Range,
}

struct Analysis {
    diagnostics: Vec<(Url, Vec<Diagnostic>)>,
    snapshot: Arc<Snapshot>,
    consumed: PendingChanges,
}

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: Mutex<Arc<State>>,
}

impl Backend {
    fn new(client: Client, root: PathBuf, config: Config) -> Self {
        Self {
            client,
            state: Mutex::new(Arc::new(State::new(root, config))),
        }
    }

    fn state(&self) -> Arc<State> {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    fn set_root(&self, root: PathBuf) {
        let root = root.canonicalize().unwrap_or(root);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.root == root {
            return;
        }
        let config = Config::load_or_default(&root).unwrap_or_else(|error| {
            tracing::warn!(error = %error, "cannot load config, using defaults");
            Config::default()
        });
        *guard = Arc::new(State::new(root, config));
    }

    fn update(&self, state: &Arc<State>, uri: Url, changes: Vec<TextDocumentContentChangeEvent>) {
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        let path = canonical_path(path);
        let Some(language) = LanguageId::from_path(&path) else {
            return;
        };
        // Serialize change application against other updates for the same file;
        // the documents map lock is only held briefly around lookup and insert
        // so the parse below never blocks readers.
        let _serialized = state
            .update_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let existing = {
            let documents = state
                .documents
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            documents.get(&path).cloned()
        };
        let Some(existing) = existing else {
            return;
        };
        let mut text = existing.file.text.as_str().to_string();
        let mut regions: Vec<(usize, usize)> = Vec::new();
        for change in &changes {
            apply_change(&mut text, change, &mut regions);
        }
        if text.len() as u64 > state.config.max_file_bytes {
            tracing::debug!(path = %path.display(), size = text.len(), "skipping large file");
            trace(&format!(
                "did_change drop large {} bytes={}",
                path.display(),
                text.len()
            ));
            let mut documents = state
                .documents
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            documents.remove(&path);
            drop(documents);
            let mut pending = state
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.mark_whole(path);
            drop(pending);
            state.bump();
            return;
        }
        trace(&format!(
            "did_change {} changes={} bytes={}",
            path.display(),
            changes.len(),
            text.len()
        ));
        let Ok(tokens) = tokenize::tokenize(&text, language) else {
            return;
        };
        let file = Arc::new(SourceFile::new(
            path.clone(),
            language,
            text,
            tokens,
            None,
            0,
        ));
        {
            let mut documents = state
                .documents
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let still_current = documents
                .get(&path)
                .is_some_and(|current| Arc::ptr_eq(current, &existing));
            if !still_current {
                return;
            }
            documents.insert(
                path.clone(),
                Arc::new(Document {
                    uri,
                    path: path.clone(),
                    file,
                }),
            );
        }
        if !regions.is_empty() {
            let mut pending = state
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (start, end) in regions {
                pending.mark_bytes(path.clone(), start, end);
            }
        }
        state.bump();
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        if let Some(root) = root_from_params(&params) {
            self.set_root(root);
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                hover_provider: Some(tower_lsp::lsp_types::HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "dup-detector".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let state = self.state();
        tracing::debug!("initialized; starting analysis task");
        spawn_analysis(self.client.clone(), state);
        self.client
            .log_message(MessageType::INFO, "dup-detector language server ready")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let state = self.state();
        let item = params.text_document;
        tracing::debug!(uri = %item.uri, "did_open");
        let Ok(path) = item.uri.to_file_path() else {
            return;
        };
        let path = canonical_path(path);
        let Some(language) = LanguageId::from_path(&path) else {
            return;
        };
        if !path.starts_with(&state.root) {
            tracing::debug!(uri = %item.uri, "ignoring file outside the workspace root");
            return;
        }
        if item.text.len() as u64 > state.config.max_file_bytes {
            tracing::debug!(uri = %item.uri, size = item.text.len(), "skipping large file");
            trace(&format!(
                "did_open skip large {} bytes={}",
                path.display(),
                item.text.len()
            ));
            return;
        }
        trace(&format!(
            "did_open {} bytes={}",
            path.display(),
            item.text.len()
        ));
        let Ok(tokens) = tokenize::tokenize(&item.text, language) else {
            return;
        };
        let file = Arc::new(SourceFile::new(
            path.clone(),
            language,
            item.text,
            tokens,
            None,
            0,
        ));
        let document = Arc::new(Document {
            uri: item.uri,
            path: path.clone(),
            file,
        });
        let mut documents = state
            .documents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        documents.insert(path.clone(), document);
        drop(documents);
        let mut pending = state
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.mark_whole(path);
        drop(pending);
        state.bump();
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let state = self.state();
        self.update(&state, params.text_document.uri, params.content_changes);
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {
        let state = self.state();
        state.bump();
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let state = self.state();
        let uri = params.text_document.uri;
        if let Ok(path) = uri.to_file_path() {
            let path = canonical_path(path);
            let mut documents = state
                .documents
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            documents.remove(&path);
            drop(documents);
            let mut pending = state
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.mark_whole(path);
        }
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
        state.bump();
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let state = self.state();
        let position = params.text_document_position_params;
        let uri = position.text_document.uri.clone();
        let pos = position.position;
        let snapshot = state
            .snapshot
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        let mut locations: Vec<Location> = Vec::new();
        for group in &snapshot.groups {
            if !group
                .occurrences
                .iter()
                .any(|occ| occ.uri == uri && range_contains(&occ.range, pos))
            {
                continue;
            }
            for occ in &group.occurrences {
                if occ.uri == uri && range_contains(&occ.range, pos) {
                    continue;
                }
                let location = Location {
                    uri: occ.uri.clone(),
                    range: occ.range,
                };
                if !locations.contains(&location) {
                    locations.push(location);
                }
            }
        }
        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GotoDefinitionResponse::Array(locations)))
        }
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let state = self.state();
        let position = params.text_document_position;
        let uri = position.text_document.uri.clone();
        let pos = position.position;
        let snapshot = state
            .snapshot
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        let mut locations: Vec<Location> = Vec::new();
        for group in &snapshot.groups {
            if !group
                .occurrences
                .iter()
                .any(|occ| occ.uri == uri && range_contains(&occ.range, pos))
            {
                continue;
            }
            for occ in &group.occurrences {
                let location = Location {
                    uri: occ.uri.clone(),
                    range: occ.range,
                };
                if !locations.contains(&location) {
                    locations.push(location);
                }
            }
        }
        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(locations))
        }
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let state = self.state();
        let position = params.text_document_position_params;
        let uri = position.text_document.uri.clone();
        let pos = position.position;
        let snapshot = state
            .snapshot
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        for group in &snapshot.groups {
            if !group
                .occurrences
                .iter()
                .any(|occ| occ.uri == uri && range_contains(&occ.range, pos))
            {
                continue;
            }
            let mut value = format!(
                "**Duplicated code** — {} tokens, {} occurrences ({})\n\n",
                group.token_count,
                group.occurrences.len(),
                clone_type_label(group.clone_type)
            );
            for occ in &group.occurrences {
                let here = occ.uri == uri;
                value.push_str(&format!(
                    "- {}{}:{}\n",
                    if here { "(here) " } else { "" },
                    occ.path.display(),
                    occ.range.start.line + 1
                ));
            }
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: None,
            }));
        }
        Ok(None)
    }
}

fn canonical_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn trace(message: &str) {
    let Ok(path) = std::env::var("DUP_DETECTOR_LSP_TRACE") else {
        return;
    };
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
            })
        })
        .map(|kb| kb / 1024)
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "[rss={rss}MB] {message}");
    }
}

fn root_from_params(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(uri) = &params.root_uri
        && let Ok(path) = uri.to_file_path()
    {
        return Some(path);
    }
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.uri.to_file_path().ok())
}

fn spawn_analysis(client: Client, state: Arc<State>) {
    tokio::spawn(async move {
        let mut analyzed = u64::MAX;
        loop {
            state.notify.notified().await;
            loop {
                let generation = state.generation.load(Ordering::SeqCst);
                tokio::time::sleep(DEBOUNCE).await;
                if state.generation.load(Ordering::SeqCst) == generation {
                    break;
                }
            }
            let generation = state.generation.load(Ordering::SeqCst);
            if generation == analyzed {
                continue;
            }
            let task_state = state.clone();
            let result = tokio::task::spawn_blocking(move || analyze(&task_state)).await;
            match result {
                Ok(Ok(Some(analysis))) => {
                    if state.generation.load(Ordering::SeqCst) != generation {
                        continue;
                    }
                    analyzed = generation;
                    let Analysis {
                        diagnostics,
                        snapshot,
                        consumed,
                    } = analysis;
                    if let Ok(mut guard) = state.snapshot.lock() {
                        *guard = snapshot;
                    }
                    if let Ok(mut live) = state.pending.lock() {
                        for (path, region) in &consumed.changed {
                            if live.changed.get(path) == Some(region) {
                                live.changed.remove(path);
                            }
                        }
                    }
                    for (uri, diagnostics) in diagnostics {
                        client.publish_diagnostics(uri, diagnostics, None).await;
                    }
                }
                Ok(Ok(None)) => analyzed = generation,
                Ok(Err(error)) => tracing::warn!(error = %error, "analysis failed"),
                Err(error) => tracing::warn!(error = %error, "analysis task failed"),
            }
        }
    });
}

fn analyze(state: &State) -> anyhow::Result<Option<Analysis>> {
    let documents: Vec<Arc<Document>> = match state.documents.lock() {
        Ok(guard) => guard.values().cloned().collect(),
        Err(poisoned) => poisoned.into_inner().values().cloned().collect(),
    };
    if documents.is_empty() {
        return Ok(None);
    }

    let config = &state.config;
    let previous = match state.snapshot.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    let open: HashSet<PathBuf> = documents.iter().map(|doc| doc.path.clone()).collect();
    let external = {
        let mut guard = state
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_mut() {
            Some(index) => index
                .refresh()
                .into_iter()
                .filter(|path| !open.contains(path))
                .collect::<Vec<_>>(),
            None => {
                let built = SourceIndex::build(&state.root, config)?;
                trace(&format!(
                    "index built root={} files={} tokens={} bytes={}",
                    state.root.display(),
                    built.files().len(),
                    built
                        .files()
                        .iter()
                        .map(|file| file.tokens.len())
                        .sum::<usize>(),
                    built
                        .files()
                        .iter()
                        .map(|file| file.text.as_bytes().len())
                        .sum::<usize>()
                ));
                *guard = Some(built);
                Vec::new()
            }
        }
    };

    let pending = {
        let mut live = state
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in external {
            live.mark_whole(path);
        }
        live.clone()
    };
    if pending.is_empty() {
        return Ok(None);
    }

    let index_guard = state
        .index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(index) = index_guard.as_ref() else {
        return Ok(None);
    };

    let overlay: HashMap<PathBuf, Arc<SourceFile>> = documents
        .iter()
        .filter(|doc| doc.path.starts_with(&state.root))
        .map(|doc| (doc.path.clone(), doc.file.clone()))
        .collect();

    let mut files: Vec<&SourceFile> = Vec::with_capacity(index.files().len() + overlay.len());
    for file in index.files() {
        if !overlay.contains_key(&file.path) {
            files.push(file);
        }
    }
    for file in overlay.values() {
        files.push(file.as_ref());
    }

    // Re-detect from two seed sources only: the windows of every clone we reported
    // last time (so unchanged matches are reproduced) and the windows touched by the
    // edits since then. Running the normal detection over just these seeds yields the
    // exact same groups as a full project scan while touching far fewer candidates.
    let mut allowed: FastSet<u64> = previous.signatures.clone();
    for (path, region) in &pending.changed {
        let file = overlay
            .get(path)
            .map(|file| file.as_ref())
            .or_else(|| index.file_by_path(path).map(|slot| &index.files()[slot]));
        let Some(file) = file else {
            continue;
        };
        match region {
            ChangedRegion::Whole => {
                allowed.extend(detect::span_window_signatures(
                    file,
                    0,
                    file.tokens.len(),
                    config,
                ));
            }
            ChangedRegion::Bytes(ranges) => {
                for &(start, end) in ranges {
                    let (start, end) = token_range_for_bytes(file, start, end, config.seed_window);
                    allowed.extend(detect::span_window_signatures(file, start, end, config));
                }
            }
        }
    }

    let groups = detect::detect_filtered(&files, config, Some(&allowed));
    trace(&format!(
        "analyze root={} files={} documents={} allowed={} previous={} changed={} groups={}",
        state.root.display(),
        files.len(),
        documents.len(),
        allowed.len(),
        previous.signatures.len(),
        pending.changed.len(),
        groups.len()
    ));
    tracing::debug!(
        allowed = allowed.len(),
        previous = previous.signatures.len(),
        changed = pending.changed.len(),
        groups = groups.len(),
        "lsp analyze"
    );
    let mut lines: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    let mut per_file: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    let mut nav_groups: Vec<NavGroup> = Vec::new();
    let mut signatures: FastSet<u64> = FastSet::default();

    for group in groups {
        let occurrences: Vec<NavOccurrence> = group
            .occurrences
            .iter()
            .map(|occ| nav_occurrence(&files, occ, &mut lines))
            .collect();
        if !occurrences
            .iter()
            .any(|occ| overlay.contains_key(&occ.path))
        {
            continue;
        }
        for occ in &group.occurrences {
            signatures.extend(detect::span_window_signatures(
                files[occ.file as usize],
                occ.start as usize,
                occ.end as usize,
                config,
            ));
        }
        for (index, occ) in occurrences.iter().enumerate() {
            if !overlay.contains_key(&occ.path) {
                continue;
            }
            let related: Vec<DiagnosticRelatedInformation> = occurrences
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| DiagnosticRelatedInformation {
                    location: Location {
                        uri: other.uri.clone(),
                        range: other.range,
                    },
                    message: format!(
                        "duplicated at {}:{}",
                        other.path.display(),
                        other.range.start.line + 1
                    ),
                })
                .collect();
            let diagnostic = Diagnostic {
                range: occ.range,
                severity: Some(DiagnosticSeverity::WARNING),
                code: None,
                code_description: None,
                source: Some(SOURCE.to_string()),
                message: format!(
                    "Duplicated code: {} tokens, {} occurrences ({}).",
                    group.token_count,
                    occurrences.len(),
                    clone_type_label(group.clone_type)
                ),
                related_information: Some(related),
                tags: None,
                data: None,
            };
            let entry = per_file.entry(occ.path.clone()).or_default();
            if entry.len() < MAX_DIAGNOSTICS_PER_FILE {
                entry.push(diagnostic);
            }
        }
        nav_groups.push(NavGroup {
            token_count: group.token_count,
            clone_type: group.clone_type,
            occurrences,
        });
    }

    tracing::debug!(
        signatures = signatures.len(),
        nav_groups = nav_groups.len(),
        "lsp snapshot"
    );
    let diagnostics = documents
        .iter()
        .filter(|doc| doc.path.starts_with(&state.root))
        .map(|doc| {
            let diagnostics = per_file.remove(&doc.path).unwrap_or_default();
            (doc.uri.clone(), diagnostics)
        })
        .collect();

    Ok(Some(Analysis {
        diagnostics,
        snapshot: Arc::new(Snapshot {
            groups: nav_groups,
            signatures,
        }),
        consumed: pending,
    }))
}

fn nav_occurrence(
    files: &[&SourceFile],
    occurrence: &Occurrence,
    lines: &mut HashMap<PathBuf, Vec<usize>>,
) -> NavOccurrence {
    let file = files[occurrence.file as usize];
    let starts = lines
        .entry(file.path.clone())
        .or_insert_with(|| build_line_starts(&file.text));
    let start_byte = file.tokens[occurrence.start as usize].start as usize;
    let end_byte = file.tokens[(occurrence.end - 1) as usize].end as usize;
    NavOccurrence {
        path: file.path.clone(),
        uri: Url::from_file_path(&file.path)
            .unwrap_or_else(|_| Url::parse("file:///").expect("static file url must parse")),
        range: Range {
            start: byte_to_position(&file.text, starts, start_byte),
            end: byte_to_position(&file.text, starts, end_byte),
        },
    }
}

fn clone_type_label(clone_type: CloneType) -> &'static str {
    match clone_type {
        CloneType::Type1 => "type-1",
        CloneType::Type2 => "type-2",
    }
}

fn range_contains(range: &Range, position: Position) -> bool {
    let after_start = position.line > range.start.line
        || (position.line == range.start.line && position.character >= range.start.character);
    let before_end = position.line < range.end.line
        || (position.line == range.end.line && position.character <= range.end.character);
    after_start && before_end
}

fn token_range_for_bytes(
    file: &SourceFile,
    start: usize,
    end: usize,
    window: usize,
) -> (usize, usize) {
    let tokens = &file.tokens;
    let mut first = tokens.partition_point(|token| (token.end as usize) <= start);
    let mut last = tokens.partition_point(|token| (token.start as usize) < end);
    if last < first {
        last = first;
    }
    first = first.saturating_sub(window);
    last = (last + window).min(tokens.len());
    (first, last)
}

fn build_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn byte_to_position(text: &str, line_starts: &[usize], byte: usize) -> Position {
    let byte = byte.min(text.len());
    let line = line_starts
        .partition_point(|&start| start <= byte)
        .saturating_sub(1);
    let line_start = line_starts.get(line).copied().unwrap_or(0);
    let character = text[line_start..byte].encode_utf16().count() as u32;
    Position {
        line: line as u32,
        character,
    }
}

fn position_to_byte(text: &str, position: Position) -> usize {
    let mut line = 0u32;
    let mut line_start = 0usize;
    if position.line > 0 {
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line += 1;
                line_start = index + 1;
                if line == position.line {
                    break;
                }
            }
        }
    }
    let mut utf16 = 0u32;
    let mut offset = line_start;
    for character in text[line_start..].chars() {
        if utf16 >= position.character || character == '\n' {
            break;
        }
        utf16 += character.len_utf16() as u32;
        offset += character.len_utf8();
    }
    offset
}

fn apply_change(
    text: &mut String,
    change: &TextDocumentContentChangeEvent,
    regions: &mut Vec<(usize, usize)>,
) {
    match &change.range {
        Some(range) => {
            let start = position_to_byte(text, range.start);
            let end = position_to_byte(text, range.end);
            if start <= end && end <= text.len() {
                text.replace_range(start..end, &change.text);
                regions.push((start, start + change.text.len()));
            }
        }
        None => {
            *text = change.text.clone();
            regions.clear();
            regions.push((0, text.len()));
        }
    }
}

fn resolve_root(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub fn run(config: Config, root: PathBuf) -> anyhow::Result<()> {
    let root = resolve_root(&root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        if std::env::var("DUP_DETECTOR_LSP_TRACE").is_ok() {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    trace("tick");
                }
            });
        }
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) =
            LspService::new(move |client| Backend::new(client, root.clone(), config.clone()));
        Server::new(stdin, stdout, socket).serve(service).await;
        Ok::<(), anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts_and_positions_round_trip() {
        let text = "fn a() {\n  let x = 1;\n}\n";
        let starts = build_line_starts(text);
        assert_eq!(starts, vec![0, 9, 22, 24]);
        let position = byte_to_position(text, &starts, 12);
        assert_eq!(
            position,
            Position {
                line: 1,
                character: 3
            }
        );
        assert_eq!(position_to_byte(text, position), 12);
    }

    #[test]
    fn positions_use_utf16_units() {
        let text = "let s = \"😀\";\nlet t = 1;\n";
        let starts = build_line_starts(text);
        let emoji_start = text.find('😀').unwrap();
        let after = byte_to_position(text, &starts, emoji_start + '😀'.len_utf8());
        assert_eq!(
            after.character,
            text[..emoji_start].encode_utf16().count() as u32 + 2
        );
    }

    #[test]
    fn applies_incremental_changes() {
        let mut text = "fn main() {}\n".to_string();
        let change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 11,
                },
                end: Position {
                    line: 0,
                    character: 11,
                },
            }),
            range_length: None,
            text: " let x = 1;".to_string(),
        };
        let mut regions = Vec::new();
        apply_change(&mut text, &change, &mut regions);
        assert_eq!(text, "fn main() { let x = 1;}\n");
        assert_eq!(regions, vec![(11, 22)]);
    }

    fn source_file(text: &str) -> SourceFile {
        let tokens = tokenize::tokenize(text, LanguageId::Rust).unwrap();
        SourceFile::new(
            PathBuf::from("test.rs"),
            LanguageId::Rust,
            text.to_string(),
            tokens,
            None,
            0,
        )
    }

    #[test]
    fn token_range_expands_around_changed_bytes() {
        let text = "fn a() { let x = 1; let y = 2; }\n";
        let file = source_file(text);
        let insert = text.find("let y").unwrap();
        let (start, end) = token_range_for_bytes(&file, insert, insert, 2);
        assert!(start < end);
        assert!(start < file.tokens.len());
        assert!(end <= file.tokens.len());
        let tokens: Vec<&str> = file.tokens[start..end]
            .iter()
            .map(|t| &text[t.start as usize..t.end as usize])
            .collect();
        assert!(tokens.contains(&"y"));
        let whole = token_range_for_bytes(&file, 0, text.len(), 2);
        assert_eq!(whole, (0, file.tokens.len()));
    }

    #[test]
    fn pending_changes_merge_regions() {
        let mut pending = PendingChanges::default();
        assert!(pending.is_empty());
        let path = PathBuf::from("a.rs");
        pending.mark_bytes(path.clone(), 0, 5);
        pending.mark_bytes(path.clone(), 10, 12);
        assert_eq!(
            pending.changed.get(&path),
            Some(&ChangedRegion::Bytes(vec![(0, 5), (10, 12)]))
        );
        pending.mark_whole(path.clone());
        assert_eq!(pending.changed.get(&path), Some(&ChangedRegion::Whole));
        pending.mark_bytes(path.clone(), 20, 25);
        assert_eq!(pending.changed.get(&path), Some(&ChangedRegion::Whole));
    }

    #[test]
    fn range_contains_position() {
        let range = Range {
            start: Position {
                line: 2,
                character: 4,
            },
            end: Position {
                line: 5,
                character: 1,
            },
        };
        assert!(range_contains(
            &range,
            Position {
                line: 2,
                character: 4
            }
        ));
        assert!(range_contains(
            &range,
            Position {
                line: 4,
                character: 0
            }
        ));
        assert!(!range_contains(
            &range,
            Position {
                line: 6,
                character: 0
            }
        ));
        assert!(!range_contains(
            &range,
            Position {
                line: 2,
                character: 3
            }
        ));
    }
}
