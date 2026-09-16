# dup-detector

English | [简体中文](README.zh-CN.md)

A tool that finds **duplicated code** in a codebase, providing LSP, MCP and CLI, aiming for parity with JetBrains IDE's _Duplicated Code_ inspection.

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
- **Fast** — parallel parsing, `.gitignore`-aware discovery, seed hashing, an in-memory index with mtime-based incremental refresh, and an on-disk token cache.
- **Three ways to use it** — a long-running MCP server for coding agents, an LSP server for editors, and a `scan` CLI.

## Supported languages

Rust, Python, JavaScript, TypeScript, TSX, C++ (`.rs`, `.py`/`.pyi`, `.js`/`.mjs`/`.cjs`/`.jsx`, `.ts`/`.mts`/`.cts`, `.tsx`, `.cpp`/`.cc`/`.cxx`/`.c++`/`.hpp`/`.hh`/`.hxx`/`.h++`/`.h`/`.ipp`/`.inl`/`.tpp`).

## Build

Requires Rust 1.98+ (edition 2024).

```bash
cargo build --release
# binary: target/release/dup-detector
```

## Quick start

```bash
# start the MCP server over stdio
dup-detector mcp

# start the language server over stdio
dup-detector lsp

# scan a path (defaults to the current directory)
dup-detector scan <path>

# JSON output, tuned thresholds
dup-detector scan <path> --json --min-lines 7 --max-groups 50
```

## Documentation

- [Configuration and CLI](docs/configuration.md) · [简体中文](docs/zh-CN/configuration.md)
- [MCP server](docs/mcp.md) · [简体中文](docs/zh-CN/mcp.md)
- [Editor integration (LSP)](docs/lsp.md) · [简体中文](docs/zh-CN/lsp.md)
- [Architecture and implementation](docs/architecture.md) · [简体中文](docs/zh-CN/architecture.md)

`AGENTS.md` documents the architecture, algorithms and coding standards in detail.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
