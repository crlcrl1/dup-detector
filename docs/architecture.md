# Architecture and Implementation

`dup-detector` is not a text/regex matcher. It tokenizes source code with
tree-sitter and detects **Type-1** and **Type-2** clones on the token stream,
using a parameterized (rename-invariant) encoding.

## Pipeline

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

## Key ideas

1. **Leaf tokens only.** Extracting tokens from the CST automatically drops
   comments and formatting and unifies languages.
2. **Parameterized encoding.** Identifiers are encoded as "distance since the
   previous occurrence" (first occurrence = 0). This makes `a=b+c` and `x=y+z`
   encode identically while still requiring structural identity.
3. **Seed-and-extend.** Fixed-length windows are hashed and bucketed; generic
   buckets (too frequent) are discarded. Candidate pairs pass a one-to-one
   **bijection check**, then extend maximally.
4. **Fixed tokens match exactly**; identifiers use the bijection; literals match
   exactly unless `parameterize_literals` is enabled.
5. **Post-processing.** Overlapping matches are merged only when file-offset
   alignment stays constant, occurrences are reported at single-semantic-unit
   granularity (multi-unit matches are split), boilerplate (imports, pure
   declarations) is filtered, and groups are sorted by size.

`AGENTS.md` contains the full specification and coding standards.

## Project layout

```
src/
  main.rs       CLI entry: `mcp` / `lsp` / `scan`
  lib.rs        library root
  config.rs     Config + dup-detector.toml loading
  language.rs   extension -> LanguageId -> tree-sitter grammar
  model.rs      Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs   source -> CST -> leaf token stream
  fast_hash.rs  fast hasher + map/set aliases
  encode.rs     token stream -> parameterized encoding
  detect.rs     seed-and-extend, bijection, clustering
  index.rs      file discovery, parallel parsing, incremental refresh
  cache.rs      on-disk token cache (mtime/size/text-hash validated, mmap'd zero-copy)
  server.rs     rmcp server + MCP tool definitions
  lsp.rs        LSP server (diagnostics, go-to-definition, references, hover)
editors/
  vscode/       VS Code extension (vscode-languageclient, packages to .vsix)
  zed/          Zed extension (wasm, registers the language server name)
tests/
  corpus.rs     corpus regression (rename / constants / added line / ...)
  corpus/       hand-crafted fixtures with precision/recall assertions
```

## Development

```bash
cargo build                          # daily build
cargo build --release                # performance-sensitive
cargo test                           # unit + corpus tests
cargo fmt                            # formatting
cargo clippy --all-targets -- -D warnings
```
