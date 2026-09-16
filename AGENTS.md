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
- MCP SDK: `rmcp` 3.3 (feat. `transport-io`, stdio transport)
- Parsing: `tree-sitter` 0.27 + grammars: rust / python / javascript / typescript / cpp
- Traversal: `ignore` (respects .gitignore), `rayon` (parallel parsing)
- Serialization: `serde` / `serde_json` / `schemars` (MCP tool schemas)
- Errors: `anyhow` (application layer) / `thiserror` (library layer)
- CLI: `clap` (derive)
- Logging: `tracing` / `tracing-subscriber` (env-filter)
- Hashing: `xxhash-rust` (xxh3)

## 3. Architecture and Module Plan

Module layout (all modules implemented; the core algorithm lives in `tokenize` / `encode` / `detect`):

```
src/
  main.rs        CLI entry: mcp / scan subcommands
  lib.rs         Library root, exports public API
  config.rs      Config (min_lines, thresholds, language toggles, etc.)
  language.rs    Extension -> LanguageId -> tree-sitter Language
  model.rs       Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs    Source file -> CST -> leaf token stream
  encode.rs      Token stream -> parameterized encoding (rename-invariant)
  detect.rs      seed-and-extend detection + bijection check + clone clustering
  index.rs       Project-level index (file discovery, parallel parsing, incremental invalidation)
  cache.rs       On-disk token cache (`.dup-detector/`, one entry per source file named by a hash of its root-relative path, mtime/size/text-hash validated)
  server.rs      rmcp ServerHandler + #[tool] tool definitions
```

Data flow:

```
File discovery (ignore) -> tree-sitter parse -> token stream -> parameterized encoding
  -> seed hash bucketing -> bijection check -> maximal match extension
  -> dedupe/merge/cluster -> sort -> MCP response
```

## 4. Core Algorithms (make-or-break, must follow)

1. **Do not use regex/text matching.** Always extract leaf tokens with tree-sitter; this naturally eliminates comments, whitespace, and formatting differences, and unifies multiple languages.
2. **Parameterized encoding**: encode identifiers by "distance since previous occurrence" (first occurrence = 0, repeats = current index - previous index; same for literals, optional toggle). This makes `a=b+c` and `x=y+z` encode identically while still requiring structural identity, avoiding over-normalization.
3. **seed-and-extend**:
   - Use fixed-length (default 8) token windows as seeds; hash and bucket them using **local** parameterized encoding within the window to guarantee rename invariance.
   - If a bucket has too many occurrences (> `max_bucket`), it is a generic pattern; discard it to reduce noise.
   - Run a **bijection check** on candidate pairs (identifiers must map one-to-one; any conflict fails), then extend maximally left/right to obtain the maximal matching span.
4. **Fixed tokens** (keywords/operators/punctuation) must match exactly; identifiers use bijection; literals match exactly by default (`parameterize_literals` can be enabled).
5. **Post-processing**: merge overlapping/adjacent matches within the same file pair only when their file-offset alignment stays constant (prevents span drift); report occurrences only at single-semantic-unit granularity — a span must be exactly one statement, item, or block (per-token unit ranges are recorded during tokenization), and multi-unit matches are split into one clone group per unit; drop groups that cannot be aligned; drop group occurrences that do not match the group representative (exact parameterized keys), so only Type-1/Type-2 clones are reported; filter boilerplate (imports, pure declarations); sort by token count; limit the number of returned groups.
7. **Same-file overlapping regions** must be handled explicitly to avoid meaningless self-overlap results.

## 5. MCP Tool Interface

The server is long-running, maintains an in-memory index keyed by workspace root, and invalidates incrementally by file mtime. All four tools are implemented in `server.rs`:

| Tool | Purpose | Key params |
| --- | --- | --- |
| `find_clones` | Project-wide duplicated code | `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file` | Clones involving a given file | `file`, `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?` |
| `find_clones_for_region` | **Most used by agents**: is the code I'm writing duplicated? | `file`, `start_line`, `end_line`, `scope?`, `min_lines?`, `max_groups?`, `types?` |
| `reindex` | Manually rebuild the index and clear its on-disk cache | `path?` |

Defaults: `min_lines = 4`, `min_occurrences = 2`, `max_groups = unlimited`, `max_bucket = 32`, seed window 8. `types` accepts `"type-1"` / `"type-2"`. `find_clones_for_region` defaults `min_lines` to the line span of the queried region and parses the file on the fly if it is not indexed yet.

Responses stay **token-efficient**: `{files_scanned, groups: [{token_count, clone_type, occurrences: [{path, start_line, end_line}]}]}`, limited in number and sorted by size; no source excerpts.

## 6. Commands

```bash
cargo build                       # daily build
cargo build --release             # performance-sensitive (always use release for large projects)
cargo run -- mcp                  # start MCP server over stdio
cargo run -- scan <path>          # scan from the command line and print results
cargo run -- scan <path> --json --min-lines 4 --min-occurrences 2 --parameterize-literals --lang rust
cargo test                        # unit/integration tests
cargo fmt                         # formatting (required before commit)
cargo clippy --all-targets -- -D warnings   # lint (required before commit)
```

Note: the `rmcp` server must not write to stdout; all logs go to stderr (set `tracing_subscriber`'s writer to stderr), otherwise the stdio protocol breaks.

## 7. Coding Standards

- Follow `rustfmt` defaults; run `cargo fmt` and `cargo clippy -- -D warnings` before committing; no warnings allowed.
- Error handling: use `anyhow` in application/CLI layers and `thiserror` for reusable library types; avoid `unwrap`/`expect` in library code (except tests).
- Do not write explanatory comments unless the logic is very counterintuitive. `///` doc comments are only for tool parameters that appear in the MCP JSON schema (visible to agents, so they serve a real purpose).
- Prefer immutability and borrowing; avoid needless `clone`; watch allocations on the detection hot path (avoid creating a `HashMap` per seed window).
- Adding a language: register the extension and grammar in `language.rs`, and add a corpus test.
- After adding/modifying an MCP tool, update Section 5 of this file accordingly.

## 8. Roadmap

- **Phase 0** Done: language scope is rust / python / javascript / typescript / tsx / cpp; defaults `min_lines = 4`, seed window 8, `max_bucket = 32`.
- **Phase 1** Done: MCP server over stdio + working `scan` CLI.
- **Phase 2** Done: tree-sitter token extraction and parameterized encoding, with unit tests.
- **Phase 3** Done: seed-and-extend detection + bijection check + maximal matching + clone clustering; `find_clones` usable.
- **Phase 4** Done: filtering/sorting/clustering for Type-1/Type-2 clones.
- **Phase 5** Done: project index with mtime/size incremental refresh and a best-effort on-disk token cache (`.dup-detector/` in the scanned root; one entry per source file, named by a hash of its root-relative path and validated by mtime/size/text hash; corrupt/missing entries fall back to parsing; refresh only rewrites changed entries and drops entries for removed files; `reindex` clears the whole directory).
- **Phase 6** Done: corpus regression under `tests/corpus/` (rename / change constant / add line / same file / unrelated) with precision/recall assertions in `tests/corpus.rs`.

## 9. Working Agreements for Agents

- Run `cargo build` first to confirm the baseline compiles.
- When done, you must run `cargo fmt` and `cargo clippy --all-targets -- -D warnings`, then `cargo test`.
- When changing public types (`model.rs`), check whether the schema in `server.rs` and the docs in Section 5 need updating.
- Do not commit unless explicitly asked. Use concise imperative commit messages.
