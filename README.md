# dup-detector

English | [简体中文](README.zh-CN.md)

A tool that finds **duplicated code** in a codebase, providing LSP, MCP and CLI, aiming for parity with JetBrains IDE's _Duplicated Code_ inspection.

Unlike text/regex based tools, `dup-detector` works on the **token stream produced by tree-sitter** and uses a **parameterized (rename-invariant) encoding**, so it recognizes code that is structurally identical even when variables or literals have been renamed.

![Duplicate code diagnostics in an editor, reported by the dup-detector LSP server](screen-shot/screen-shot.png)

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

## Download

Download the archive for your platform from [GitHub Releases](https://github.com/crlcrl1/dup-detector/releases), extract it, and place `dup-detector` (or `dup-detector.exe` on Windows) in a directory on your `PATH`.

| Platform | Architecture | Release asset |
| --- | --- | --- |
| Linux (GNU, built on Ubuntu 22.04) | x86_64 | `dup-detector-x86_64-unknown-linux-gnu.tar.gz` |
| Linux (GNU, built on Ubuntu 22.04) | ARM64 | `dup-detector-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Intel | `dup-detector-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `dup-detector-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `dup-detector-x86_64-pc-windows-msvc.zip` |
| Windows | ARM64 | `dup-detector-aarch64-pc-windows-msvc.zip` |

Each archive includes the binary, license, and READMEs. `SHA256SUMS` contains the SHA-256 checksum of every archive. On Linux, place it alongside the downloaded archives and run `sha256sum --check --ignore-missing SHA256SUMS`.

Publishing a release or prerelease runs the [release workflow](.github/workflows/release.yml), which builds the tagged source with Rust 1.98.1 and `Cargo.lock` on all six platforms, checks that each binary starts and scans a sample, and uploads the archives and checksums once every build succeeds. Include the workflow in the release tag. Only publishing the release triggers this workflow; creating a draft or pushing a tag alone does not.

To rebuild an existing release, open **Actions → Release binaries → Run workflow** and enter its tag in the `tag` field. The manual workflow must be present on the default branch; it checks out the supplied tag and replaces assets with the same names. Uploads use the built-in `GITHUB_TOKEN`; no additional secret is needed.

## Build from source

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
