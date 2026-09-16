# dup-detector for VS Code

A VS Code extension that launches the `dup-detector` language server and shows
duplicated code as `Warning` diagnostics. Go to definition (`F12`) inside a clone
jumps to the other occurrences, and find all references (`Shift+F12`) lists them.

## Requirements

Build the `dup-detector` binary (repository root):

```sh
cargo build --release
# or install it on your PATH:
cargo install --path /path/to/dup-detector
```

## Build and run

```sh
npm install
npm run compile
```

Then either:

- open this directory (`editors/vscode`) in VS Code and press `F5` to launch an
  Extension Development Host, or
- package it and install the `.vsix`:

  ```sh
  npm run package
  code --install-extension dup-detector-0.1.0.vsix
  ```

## Settings

| Setting                 | Default        | Description                                                        |
| ----------------------- | -------------- | ------------------------------------------------------------------ |
| `dup-detector.serverPath` | `dup-detector` | Path to the executable. It is launched with the `lsp` argument.    |

If the binary is not on your `PATH`, set an absolute path:

```json
{
  "dup-detector.serverPath": "/path/to/dup-detector/target/release/dup-detector"
}
```

## Supported languages

Rust, Python, JavaScript, TypeScript, TSX, C and C++.
