# AGENTS.md

This document is the development guide for coding agents working on this project. Read it fully before changing any code.

## 1. Project Overview

`dup-detector` is an **MCP (Model Context Protocol) server** for coding agents that detects duplicated code in a codebase, aiming for parity with JetBrains IDE's "Duplicated Code" inspection.

The core goal is not text-level matching but **semantic (rename-invariant) detection**: recognizing code that is structurally identical even when a variable or two has been renamed.

### Clone Types (Targets)

| Type | Meaning | Target |
| --- | --- | --- |
| Type-1 | Identical except whitespace/comments/formatting | Yes |
| Type-2 | Identifiers/literals consistently renamed | **Core target** |

### Reference Implementations (for comparison/inspiration, do not copy directly)

- IntelliJ `DuplicatedCodeInspection`
- CCFinder / CCFinderSW (token + suffix tree)
- SourcererCC (bag-of-tokens + inverted index, scalable)
- NiCad (AST + pretty-print + diff)
- Baker's parameterized matching (p-string)

## 2. Tech Stack

Already written into `Cargo.toml`; no need to pick versions again:

- Rust **edition 2024**, toolchain 1.98+
- MCP SDK: `rmcp` 3.5 (feat. `transport-io`, stdio transport)
- LSP SDK: `tower-lsp` 0.20 (stdio transport, tokio runtime)
- Parsing: `tree-sitter` 0.27 + grammars: rust / python / javascript / typescript / c / cpp (`tree-sitter-language` for dynamically loaded grammars)
- Grammar plugins: `libloading` (dlopen tree-sitter grammar shared libraries at startup)
- Traversal: `ignore` (respects .gitignore), `rayon` (parallel parsing)
- Serialization: `serde` / `serde_json` / `schemars` (MCP tool schemas), `toml` (config file)
- Errors: `anyhow` (application layer) / `thiserror` (library layer)
- CLI: `clap` (derive)
- Platform paths: `dirs` (user config/cache directories)
- Logging: `tracing` / `tracing-subscriber` (env-filter)
- Hashing: `xxhash-rust` (xxh3)

## 3. Architecture and Module Plan

Module layout (all modules implemented; the core algorithm lives in `tokenize` / `encode` / `detect`):

```
src/
  main.rs        CLI entry: mcp / lsp / scan / languages subcommands
  lib.rs         Library root, exports public API
  config.rs      Config (min_lines, thresholds, language toggles, etc.) + user-level/project-level `dup-detector.toml` loading
  language.rs    Global `LanguageRegistry`: LanguageId handle -> LanguageDef (name, extensions, tree-sitter grammar, kind classification tables, fingerprint); built-ins registered at startup, plugins add more
  plugin.rs      Grammar plugin loader: scans the user-level `grammars/` directory for `grammar.toml` manifests + grammar shared libraries, loads them into the registry (best-effort, failures only warn)
  install.rs     `dup-detector install` support: clone a tree-sitter grammar repo, compile it into a shared library and write its `grammar.toml` manifest into the user-level `grammars/` directory
  doctor.rs      `dup-detector doctor`: health checks for the plugin toolchain (git, C compiler probe build, writable grammars dir) and per-plugin load validation
  model.rs       Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs    Source file -> CST -> leaf token stream
  fast_hash.rs   FastHasher (xxh3-based) + FastMap / FastSet aliases
  encode.rs      Token stream -> parameterized encoding (rename-invariant)
  detect.rs      seed-and-extend detection + bijection check + clone clustering
  index.rs       Project-level index (file discovery, parallel parsing, incremental invalidation)
  cache.rs       On-disk token cache (project `.dup-detector/` or, with `cache_location = "user-cache"`, `<user cache dir>/dup-detector/<hash of root>`; one entry per source file named by a hash of its cache-base-relative path — the index root, or the server's startup directory when scope indexes share one cache — mtime/size/text-hash validated plus a `grammar_fingerprint` so grammar upgrades or plugin manifest changes invalidate stale tokens; entries are flat little-endian POD arrays (`Token`/hash) mapped zero-copy via `memmap2` on big-endian fallback decoded)
  server.rs      rmcp ServerHandler + #[tool] tool definitions
  lsp.rs         tower-lsp LanguageServer (diagnostics, definition, references, hover)
editors/
  vscode/        VS Code extension (vscode-languageclient, packages to .vsix)
  zed/           Zed extension (wasm, registers the language server name)
docs/
  *.md           user documentation (configuration, mcp, lsp, architecture)
  zh-CN/*.md     Chinese translations
```

Data flow:

```
File discovery (ignore) -> tree-sitter parse -> token stream -> parameterized encoding
  -> seed hash bucketing -> bijection check -> maximal match extension
  -> dedupe/merge/cluster -> sort -> MCP response
```

### Configuration file

Two `dup-detector.toml` levels (constant `config::CONFIG_FILE_NAME`) are merged key by key, project-level winning: the user-level file at `user_config_path()` (`~/.config/dup-detector/dup-detector.toml` on Linux, `~/Library/Application Support/...` on macOS, `%APPDATA%\...` on Windows) and the nearest project-level file found by searching upwards from the starting path to the filesystem root (`scan` starts at the scanned path, `mcp` at the server's working directory; the server also searches each scope/index root and falls back to the startup config when a root has none). The `mcp` and `lsp` servers watch both files via `Config::source_stamps` and reload when either appears, disappears or changes, rebuilding the affected index. All keys are optional; a missing file yields `Config::default()` (and `Config::load` returns `None` when neither level exists). Unknown keys, unknown language names, and degenerate values (`0` for `min_lines`/`min_occurrences`/`max_bucket`/`seed_window`/`max_file_bytes`, or `max_file_bytes` above 4 GiB) are errors (`ConfigError`). Hidden (dot) directories are skipped unless `include_hidden = true`. CLI flags and MCP tool params override the files via `with_limits`/`with_max_file_bytes` (both validate and return `Result`) or direct field assignment. Schema: `min_lines`, `min_occurrences`, `max_bucket`, `seed_window`, `max_groups`, `parameterize_literals`, `languages` (array of language names accepted by `LanguageId::from_name`), `no_ignore` (default off; also scan files excluded by .gitignore/.ignore rules), `include_hidden` (default off; hidden dot-directories are otherwise skipped like ripgrep), `max_file_bytes` (default 2 MiB; larger files are skipped by the index and by LSP overlays to bound parse memory), `filter_boilerplate` (default on; drop clone groups whose occurrences contain no logic markers), `cache_location` (`"project"` default, `.dup-detector/` at the root, or `"user-cache"`, a per-root directory under the platform user cache dir; `SourceIndex::clear_cache` clears both).

### LSP server

`lsp.rs` implements `tower_lsp::LanguageServer` over stdio (`dup-detector lsp`). It publishes clone occurrences in open documents as `Warning` diagnostics and answers `textDocument/definition`, `textDocument/references` and `textDocument/hover` from the last analysis snapshot. Editor buffers (including unsaved edits) are overlaid on the disk index by tokenizing the changed document in memory. Efficiency rules, keep them intact: incremental text sync (only the changed document is re-tokenized), a 350 ms debounce with coalescing and generation-based cancellation of stale runs, analysis on `spawn_blocking`, and incremental re-detection where `allowed` seeds are the changed regions plus the window signatures of the clones reported in the previous snapshot (so unchanged clones are reproduced and the candidate set scales with edits, not project size). A `PendingChanges` set tracks touched byte ranges; externally changed non-open files are treated as whole-file changes. Open documents of languages disabled by `languages` are ignored, mirroring the index, and the user-level and project-level config files are re-read when either changes. The index does mtime/size incremental refresh and diagnostics are capped per file.

Correctness invariant: for every clone whose occurrences include an open file, the incremental result must equal a full `detect_filtered` restricted to the open files. This is validated by the `incremental_analysis_matches_full_detection_after_edits` test in `lsp.rs`, which applies synthetic in-buffer and on-disk edits and compares the incremental snapshot against a full `detect_filtered` over the merged file set; keep it passing when touching the seed logic.

## 4. Core Algorithms (make-or-break, must follow)

1. **Do not use regex/text matching.** Always extract leaf tokens with tree-sitter; this naturally eliminates comments, whitespace, and formatting differences, and unifies multiple languages.
2. **Parameterized encoding**: encode identifiers by "distance since previous occurrence" (first occurrence = 0, repeats = current index - previous index; same for literals, optional toggle). This makes `a=b+c` and `x=y+z` encode identically while still requiring structural identity, avoiding over-normalization.
3. **seed-and-extend**:
   - Use fixed-length (default 8) token windows as seeds; hash and bucket them using **local** parameterized encoding within the window to guarantee rename invariance.
   - If a bucket has too many occurrences (> `max_bucket`), it is a generic pattern; discard it to reduce noise.
   - Run a **bijection check** on candidate pairs (identifiers must map one-to-one; any conflict fails), then extend maximally left/right to obtain the maximal matching span.
4. **Fixed tokens** (keywords/operators/punctuation) must match exactly; identifiers use bijection; literals match exactly by default (`parameterize_literals` can be enabled).
5. **Post-processing**: merge overlapping/adjacent matches within the same file pair only when their file-offset alignment stays constant (prevents span drift); report occurrences only at single-semantic-unit granularity — a span must be exactly one statement, item, or block (per-token unit ranges are recorded during tokenization), and multi-unit matches are split into one clone group per unit; drop groups that cannot be aligned; drop group occurrences that do not match the group representative (exact parameterized keys), so only Type-1/Type-2 clones are reported; filter boilerplate (imports, pure declarations) unless `filter_boilerplate` is disabled; sort by token count; limit the number of returned groups.
7. **Same-file overlapping regions** must be handled explicitly to avoid meaningless self-overlap results.

## 5. MCP Tool Interface

The server is long-running, maintains an in-memory index keyed by workspace root, and invalidates incrementally by file mtime. A `scope` must be the server's startup directory or one of its subdirectories (resolved after canonicalization; anything outside is rejected), and the on-disk token cache always lives at the startup directory (`<startup dir>/.dup-detector` by default, or the user cache dir keyed by the startup dir), never inside a scope subdirectory. Cache entries are keyed by startup-directory-relative paths, so all scopes of one workspace share a single entry per file (older per-scope `.dup-detector/` leftovers are removed on the next build). All four tools are implemented in `server.rs`:

| Tool | Purpose | Key params |
| --- | --- | --- |
| `find_clones` | Project-wide duplicated code | `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file` | Clones involving a given file | `file`, `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?` |
| `find_clones_for_region` | **Most used by agents**: is the code I'm writing duplicated? | `file`, `start_line`, `end_line`, `scope?`, `min_lines?`, `max_groups?`, `types?` |
| `reindex` | Drop all in-memory indexes and clear the startup directory's on-disk cache (rebuilt on next scan) | (none) |

Defaults: `min_lines = 7`, `min_occurrences = 2`, `max_groups = unlimited`, `max_bucket = 32`, seed window 8. `types` accepts `"type-1"` / `"type-2"`. `find_clones_for_region` defaults `min_lines` to `min(config.min_lines, line span of the queried region)` and parses the file on the fly if it is not indexed yet; `file` must resolve inside `scope`.

Responses stay **token-efficient**: `{files_scanned, groups: [{token_count, clone_type, occurrences: [{path, start_line, end_line}]}]}`, limited in number and sorted by size; no source excerpts.

## 6. Commands

```bash
cargo build                       # daily build
cargo build --release             # performance-sensitive (always use release for large projects)
cargo run -- mcp                  # start MCP server over stdio
cargo run -- lsp                  # start language server over stdio
cargo run -- scan <path>          # scan from the command line and print results
cargo run -- scan <path> --json --min-lines 7 --min-occurrences 2 --parameterize-literals --lang rust
cargo run -- languages            # list registered languages (built-in + plugins)
cargo run -- install go           # download tree-sitter-go, compile and install it as a plugin
cargo run -- doctor               # check plugin toolchain dependencies and installed plugins (exit 1 on failure)
cargo test                        # unit/integration tests
cargo fmt                         # formatting (required before commit)
cargo clippy --all-targets -- -D warnings   # lint (required before commit)
cargo bench                        # criterion micro-benchmarks (`-- --save-baseline <name>` keeps a comparison baseline)
```

Note: the `rmcp` and LSP servers must not write to stdout; all logs go to stderr (set `tracing_subscriber`'s writer to stderr), otherwise the stdio protocol breaks.

## 7. Coding Standards

- Follow `rustfmt` defaults; run `cargo fmt` and `cargo clippy -- -D warnings` before committing; no warnings allowed.
- Error handling: use `anyhow` in application/CLI layers and `thiserror` for reusable library types; avoid `unwrap`/`expect` in library code (except tests).
- Do not write explanatory comments unless the logic is very counterintuitive. `///` doc comments are only for tool parameters that appear in the MCP JSON schema (visible to agents, so they serve a real purpose).
- Prefer immutability and borrowing; avoid needless `clone`; watch allocations on the detection hot path (avoid creating a `HashMap` per seed window).
- Adding a language (built-in): register the extension and grammar in `language.rs`, and add a corpus test.
- Adding a language (plugin, no recompile): `dup-detector install <name>` (or `org/repo` / URL; `-e/--extension` to override extensions, `--force` to overwrite) clones the tree-sitter grammar, compiles it and writes the manifest into the user-level `grammars/` directory; `dup-detector languages` verifies registration. Manual installs follow the layout in the `plugin.rs` module docs. Plugin grammars are native code — only load trusted libraries.
- After adding/modifying an MCP tool, update Section 5 of this file accordingly.

## 8. Roadmap

- **Phase 0** Done: language scope is rust / python / javascript / typescript / tsx / c / cpp; defaults `min_lines = 7`, seed window 8, `max_bucket = 32`.
- **Phase 1** Done: MCP server over stdio + working `scan` CLI.
- **Phase 2** Done: tree-sitter token extraction and parameterized encoding, with unit tests.
- **Phase 3** Done: seed-and-extend detection + bijection check + maximal matching + clone clustering; `find_clones` usable.
- **Phase 4** Done: filtering/sorting/clustering for Type-1/Type-2 clones.
- **Phase 5** Done: project index with mtime/size incremental refresh and a best-effort on-disk token cache (`.dup-detector/` in the scanned root by default, or a per-root directory under the user cache dir when `cache_location = "user-cache"`; one entry per source file, named by a hash of its cache-base-relative path and validated by mtime/size/text hash; corrupt/missing entries fall back to parsing; refresh only rewrites changed entries and drops entries for removed files; MCP scope indexes key entries by startup-directory-relative path so all scopes of one workspace share a single entry per file; `reindex` clears both locations).
- **Phase 6** Done: corpus regression under `tests/corpus/` (rename / change constant / add line / same file / unrelated) with precision/recall assertions in `tests/corpus.rs`.
- **Phase 7** Done: `lsp.rs` LSP server over stdio publishing clone warnings for open documents plus go-to-definition / references / hover navigation, with incremental sync, debouncing, generation-based cancellation and open-document seed filtering.

## 9. Working Agreements for Agents

- Run `cargo build` first to confirm the baseline compiles.
- When done, you must run `cargo fmt` and `cargo clippy --all-targets -- -D warnings`, then `cargo test`.
- When changing public types (`model.rs`), check whether the schema in `server.rs` and the docs in Section 5 need updating.
- Do not commit unless explicitly asked. Use concise imperative commit messages.
