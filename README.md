# dup-detector

English | [简体中文](README.zh-CN.md)

An [MCP](https://modelcontextprotocol.io) server (and CLI) that finds **duplicated code** in a codebase, aiming for parity with JetBrains IDE's _Duplicated Code_ inspection.

Unlike text/regex based tools, `dup-detector` works on the **token stream produced by tree-sitter** and uses a **parameterized (rename-invariant) encoding**, so it recognizes code that is structurally identical even when variables or literals have been renamed.

## Clone types

| Type   | Meaning                                                    | Supported         |
| ------ | ---------------------------------------------------------- | ----------------- |
| Type-1 | Identical except whitespace / comments / formatting        | Yes               |
| Type-2 | Identifiers (and optionally literals) consistently renamed | Yes (core target) |

## Highlights

- **Semantic, not textual** — comments, whitespace and formatting never matter.
- **Rename invariant** — `a = b + c` and `x = y + z` are the same clone.
- **Multi-language** — Rust, Python, JavaScript, TypeScript/TSX, C++.
- **Fast** — parallel parsing (`rayon`), `.gitignore`-aware discovery (`ignore`), hashed seed buckets, in-memory index with mtime-based incremental refresh, and an on-disk token cache that survives restarts.
- **Token-efficient responses** — file paths + line ranges + size + clone type, no source excerpts.
- **Two ways to use it** — a long-running MCP server for coding agents, and a `scan` CLI.

## How it works

```
file discovery (.gitignore aware)
  -> tree-sitter parse          (leaf tokens only: identifiers, literals, fixed)
  -> parameterized encoding     (identifiers encoded by distance to previous occurrence)
  -> seed hashing               (fixed-length token windows, default 8)
  -> bijection check            (identifiers must map one-to-one)
  -> maximal match extension
  -> cluster / merge / sort     (dedupe, filter, rank by token count)
  -> result
```

Key ideas (see `AGENTS.md` for the full spec):

1. **Leaf tokens only.** Extracting tokens from the CST automatically drops comments and formatting and unifies languages.
2. **Parameterized encoding.** Identifiers are encoded as "distance since the previous occurrence" (first occurrence = 0). This makes `a=b+c` and `x=y+z` encode identically while still requiring structural identity.
3. **Seed-and-extend.** Fixed-length windows are hashed and bucketed; generic buckets (too frequent) are discarded. Candidate pairs pass a one-to-one **bijection check**, then extend maximally.
4. **Fixed tokens match exactly**; identifiers use the bijection; literals match exactly unless `parameterize_literals` is enabled.

## Supported languages

Rust, Python, JavaScript, TypeScript, TSX, C++ (`.rs`, `.py`/`.pyi`, `.js`/`.mjs`/`.cjs`/`.jsx`, `.ts`/`.mts`/`.cts`, `.tsx`, `.cpp`/`.cc`/`.cxx`/`.c++`/`.hpp`/`.hh`/`.hxx`/`.h++`/`.h`/`.ipp`/`.inl`/`.tpp`).

## Build

Requires Rust 1.98+ (edition 2024).

```bash
cargo build --release
# binary: target/release/dup-detector
```

## CLI usage

```bash
# start the MCP server over stdio
dup-detector mcp

# scan a path (defaults to the current directory)
dup-detector scan <path>

# restrict languages and tune thresholds
dup-detector scan <path> --lang rust --lang python --min-lines 5 --min-occurrences 2 --max-groups 50

# JSON output (for tooling)
dup-detector scan <path> --json

# allow consistent literal renames to match
dup-detector scan <path> --parameterize-literals
```

Scan options:

| Flag                      | Default | Description                                  |
| ------------------------- | ------- | -------------------------------------------- |
| `--min-lines <N>`         | `5`     | Minimum line count for a clone group         |
| `--min-occurrences <N>`   | `2`     | Minimum occurrences per group                |
| `--max-groups <N>`        | none    | Maximum number of groups to report           |
| `--parameterize-literals` | off     | Treat consistently renamed literals as equal |
| `--lang <LANG>`           | all     | Restrict to a language (repeatable)          |
| `--json`                  | off     | Print results as JSON                        |

## Configuration

Configuration is per project: put a `dup-detector.toml` at the project root. It is read at startup by searching upwards from the scanned path for `scan`, and from the server's working directory and each `scope` root for the MCP server, so scanning a subdirectory still finds the project config. Command-line flags and MCP tool parameters override the file. Every key is optional and falls back to the default.

```toml
# dup-detector.toml
min_lines = 5
min_occurrences = 2
max_bucket = 32          # seed buckets larger than this are dropped as generic
seed_window = 8          # seed window length in tokens
max_groups = 100         # omit / comment out for no limit
parameterize_literals = false
languages = ["rust", "python", "javascript", "typescript", "tsx", "cpp"]
```

Unknown keys and unknown language names are reported as errors. A missing `dup-detector.toml` is fine and simply uses the defaults.

## Using it as an MCP server

Register the release binary with your MCP client. Example configuration:

```json
{
  "mcpServers": {
    "dup-detector": {
      "command": "/absolute/path/to/dup-detector",
      "args": ["mcp"]
    }
  }
}
```

The server keeps an in-memory index per workspace root and refreshes it incrementally by file mtime/size. Each root's `dup-detector.toml` supplies its configuration, falling back to the server's startup directory. Parsed token streams are also cached in the `.dup-detector/` directory at the scanned root, one entry per source file named by a hash of its root-relative path (validated by mtime, size and a text hash), so new processes only reparse changed files and only changed entries are rewritten; use `reindex` to clear the directory. Add `.dup-detector/` to `.gitignore` if you don't want it tracked. All logs go to stderr so the stdio protocol stays clean.

### Tools

| Tool                     | Purpose                               | Parameters                                                                                               |
| ------------------------ | ------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `find_clones`            | Project-wide duplicated code          | `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file`    | Clones involving a given file         | `file`, `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`                             |
| `find_clones_for_region` | "Is the code I'm writing duplicated?" | `file`, `start_line`, `end_line`, `scope?`, `min_lines?`, `max_groups?`, `types?`                       |
| `reindex`                | Rebuild the in-memory index           | `path?`                                                                                                  |

- `scope` defaults to the current working directory.
- `types` accepts `"type-1"`, `"type-2"`.
- `find_clones_for_region` defaults `min_lines` to the line span of the queried region and parses the file on the fly if it is not indexed yet.

### Response shape

```json
{
  "files_scanned": 42,
  "groups": [
    {
      "token_count": 124,
      "clone_type": "type-2",
      "occurrences": [
        { "path": "src/a.rs", "start_line": 10, "end_line": 32 },
        { "path": "src/b.rs", "start_line": 4, "end_line": 26 }
      ]
    }
  ]
}
```

## Project layout

```
src/
  main.rs       CLI entry: `mcp` / `scan`
  lib.rs        library root
  config.rs     Config + dup-detector.toml loading
  language.rs   extension -> LanguageId -> tree-sitter grammar
  model.rs      Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs   source -> CST -> leaf token stream
  encode.rs     token stream -> parameterized encoding
  detect.rs     seed-and-extend, bijection, clustering
  index.rs      file discovery, parallel parsing, incremental refresh
  cache.rs      on-disk token cache (mtime/size/text-hash validated)
  server.rs     rmcp server + MCP tool definitions
tests/
  corpus.rs     corpus regression (rename / constants / added line / ...)
  corpus/       hand-crafted fixtures with precision/recall assertions
```

## Development

```bash
cargo build
cargo test          # unit + corpus tests
cargo fmt           # formatting
cargo clippy --all-targets -- -D warnings
```

`AGENTS.md` documents the architecture, algorithms and coding standards in detail.

## Roadmap

- Phases 0–6 done: tokens, encoding, detection, Type-1/2, in-memory index with mtime-based refresh plus an on-disk token cache, corpus regression.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
