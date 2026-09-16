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
- **Fast** — parallel parsing (`rayon`), `.gitignore`-aware discovery (`ignore`), hashed seed buckets, in-memory index with mtime-based incremental refresh.
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
dup-detector scan <path> --lang rust --lang python --min-tokens 50 --min-occurrences 2 --max-groups 50

# JSON output (for tooling)
dup-detector scan <path> --json

# allow consistent literal renames to match
dup-detector scan <path> --parameterize-literals
```

Scan options:

| Flag                      | Default | Description                                  |
| ------------------------- | ------- | -------------------------------------------- |
| `--min-tokens <N>`        | `40`    | Minimum token count for a clone group        |
| `--min-occurrences <N>`   | `2`     | Minimum occurrences per group                |
| `--max-groups <N>`        | `50`    | Maximum number of groups to report           |
| `--parameterize-literals` | off     | Treat consistently renamed literals as equal |
| `--lang <LANG>`           | all     | Restrict to a language (repeatable)          |
| `--json`                  | off     | Print results as JSON                        |

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

The server keeps an in-memory index per workspace root and refreshes it incrementally by file mtime/size. All logs go to stderr so the stdio protocol stays clean.

### Tools

| Tool                     | Purpose                               | Parameters                                                                                               |
| ------------------------ | ------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `find_clones`            | Project-wide duplicated code          | `scope?`, `min_tokens?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file`    | Clones involving a given file         | `file`, `scope?`, `min_tokens?`, `min_occurrences?`, `max_groups?`, `types?`                             |
| `find_clones_for_region` | "Is the code I'm writing duplicated?" | `file`, `start_line`, `end_line`, `scope?`, `min_tokens?`, `max_groups?`, `types?`                       |
| `reindex`                | Rebuild the in-memory index           | `path?`                                                                                                  |

- `scope` defaults to the current working directory.
- `types` accepts `"type-1"`, `"type-2"`.
- `find_clones_for_region` defaults `min_tokens` to the size of the queried region and parses the file on the fly if it is not indexed yet.

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
  config.rs     Config (thresholds, language toggles)
  language.rs   extension -> LanguageId -> tree-sitter grammar
  model.rs      Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs   source -> CST -> leaf token stream
  encode.rs     token stream -> parameterized encoding
  detect.rs     seed-and-extend, bijection, clustering
  index.rs      file discovery, parallel parsing, incremental refresh
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

- Phases 0–4, 6 done: tokens, encoding, detection, Type-1/2, corpus regression.
- **Phase 5 (partial)**: in-memory index with mtime-based refresh; an on-disk cache is not implemented yet.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
