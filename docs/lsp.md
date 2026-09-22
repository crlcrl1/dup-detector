# Editor integration (LSP)

Run `dup-detector lsp` as a language server to surface duplicates directly in
the editor:

- **Warnings** — every clone occurrence in an open file is reported as a
  `Warning` diagnostic whose range covers the duplicated span and whose message
  lists the other occurrences (click-through via related information).
- **Jump to duplicates** — go to definition on any line of a clone jumps to the
  other occurrences; find references lists all of them.
- **Hover** — shows the clone size, type and every occurrence location.

Example editor configuration (Neovim):

```lua
vim.lsp.start({
  name = "dup-detector",
  cmd = { "/absolute/path/to/dup-detector", "lsp" },
  root_dir = vim.fs.root(0, { "dup-detector.toml", ".git" }),
})
```

The workspace's project config (`dup-detector.toml`) is merged with the
user-level config (the server also falls back to its startup directory); see
[Configuration](configuration.md).

## Bundled editor plugins

Ready-to-install plugins live under [`editors/`](../editors/):

- [`editors/vscode`](../editors/vscode) — a VS Code extension (compiles the LSP
  client and packages to `.vsix`).
- [`editors/zed`](../editors/zed) — a Zed extension. Zed cannot launch an
  arbitrary language server from settings alone (the server name must be
  registered by an extension), so this registers `dup-detector` for the
  supported languages.

## How the LSP stays fast

Detection is project-wide, so re-running it on every keystroke would be
wasteful. The server:

- uses **incremental text sync** and only re-tokenizes the document that changed;
- **debounces** edits (350 ms) and coalesces bursts, cancelling stale runs via a
  generation counter;
- runs the CPU-heavy analysis on a **blocking thread pool**, so request handling
  stays responsive;
- is **incremental per edit**: it re-detects only the token windows touched by
  the edit plus the window signatures of the clones reported last time, so
  unchanged clones are reproduced exactly and the candidate set stays
  proportional to the changes rather than the project size;
- reuses the in-memory index with **mtime/size incremental refresh** and the
  per-file seed-signature cache;
- caps published diagnostics per file (`100`) to avoid flooding.
