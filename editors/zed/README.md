# dup-detector for Zed

A minimal [Zed](https://zed.dev) extension that registers the `dup-detector`
language server so its `Warning` diagnostics and go-to-definition/references
navigation work in Zed.

Zed does not allow adding an arbitrary language server from settings alone: the
server name must be registered by an extension. This extension does exactly
that.

## Requirements

- The `dup-detector` binary (build it with `cargo build --release` in the
  repository root). Put it on your `PATH`, e.g.:

  ```sh
  cargo install --path /path/to/dup-detector
  ```

  or point Zed at it explicitly via `lsp.dup-detector.binary` (see below).
- The `wasm32-wasip1` Rust target: `rustup target add wasm32-wasip1`.

## Install

1. In Zed, run `zed: install dev extension` and select this directory
   (`editors/zed`). Zed compiles the extension to WebAssembly.
2. Enable the server per language in `~/.config/zed/settings.json`:

   ```json
   {
     "languages": {
       "Rust": { "language_servers": ["dup-detector", "..."] },
       "Python": { "language_servers": ["dup-detector", "..."] },
       "TypeScript": { "language_servers": ["dup-detector", "..."] },
       "TSX": { "language_servers": ["dup-detector", "..."] }
     }
   }
   ```

   `"..."` keeps the language's other servers enabled; drop it if you want only
   dup-detector.

3. Optional — use an explicit binary path:

   ```json
   {
     "lsp": {
       "dup-detector": {
         "binary": {
           "path": "/path/to/dup-detector",
           "arguments": ["lsp"]
         }
       }
     }
   }
   ```

## Notes

- The extension returns `dup-detector` (resolved from the worktree `PATH`) with
  the `lsp` argument.
- Diagnostics appear as warnings; use `editor: go to definition` (`f12`) inside a
  clone to jump to its other occurrences.
