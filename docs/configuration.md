# Configuration and CLI

## Configuration files (`dup-detector.toml`)

The same file name is read from two levels and merged key by key:

- **User-level**: `~/.config/dup-detector/dup-detector.toml` on Linux (or
  `$XDG_CONFIG_HOME/dup-detector/dup-detector.toml` when `XDG_CONFIG_HOME` is
  set), `~/Library/Application
  Support/dup-detector/dup-detector.toml` on macOS and
  `%APPDATA%\dup-detector\dup-detector.toml` on Windows. It applies to every
  project on the machine.
- **Project-level**: put a `dup-detector.toml` at the project root. It is read
  at startup by searching upwards from the scanned path for `scan`, and from the
  server's working directory and each `scope` root for the MCP server, so
  scanning a subdirectory still finds the project config.

Keys present in the project-level file win; the remaining keys fall back to the
user-level file and then to the built-in defaults. The `mcp` and `lsp` servers
watch both files and reload them when they change, rebuilding the affected
index. Command-line flags and MCP tool parameters override both files. Every key
is optional.

```toml
# ~/.config/dup-detector/dup-detector.toml  (or <project>/dup-detector.toml)
min_lines = 7
min_occurrences = 2
max_bucket = 32          # seed buckets larger than this are dropped as generic
seed_window = 8          # seed window length in tokens
max_groups = 100         # omit / comment out for no limit
parameterize_literals = false
languages = ["rust", "python", "javascript", "typescript", "tsx", "cpp"]
no_ignore = false         # also scan files excluded by .gitignore/.ignore rules
include_hidden = false    # also scan dot-directories like .github (off by default)
max_file_bytes = 2097152   # skip files larger than 2 MiB
filter_boilerplate = true  # drop clones that are pure declarations without logic
cache_location = "project" # "project" (.dup-detector/ at the root) or "user-cache"
```

| Key                     | Default | Meaning                                                            |
| ----------------------- | ------- | ------------------------------------------------------------------ |
| `min_lines`             | `7`     | Minimum line count for a clone group                               |
| `min_occurrences`       | `2`     | Minimum occurrences per group                                      |
| `max_bucket`            | `32`    | Seed buckets larger than this are treated as generic and discarded |
| `seed_window`           | `8`     | Seed window length in tokens                                       |
| `max_groups`            | none    | Maximum number of groups to report                                 |
| `parameterize_literals` | `false` | Treat consistently renamed literals as equal                       |
| `languages`             | all     | Languages to scan (`LanguageId::from_name` names)                  |
| `no_ignore`             | `false` | Also scan files excluded by `.gitignore`/`.ignore` rules           |
| `include_hidden`        | `false` | Also scan hidden (dot) directories, e.g. `.github`                 |
| `max_file_bytes`        | `2097152` | Files larger than this are skipped (bounds parse memory)         |
| `filter_boilerplate`    | `true`  | Drop clone groups that are pure declarations without logic markers |
| `cache_location`        | `project` | Where parsed token caches live: `project` or `user-cache`          |

`cache_location = "project"` keeps one entry per source file in `<project
root>/.dup-detector/`. `"user-cache"` stores them under the platform user cache
directory instead — `~/.cache/dup-detector/<hash of the root>` on Linux,
`~/Library/Caches/dup-detector/<hash>` on macOS, `%LOCALAPPDATA%\dup-detector\<hash>`
on Windows — which keeps the project tree clean. The `reindex` tool clears both
locations, so switching the setting never leaves stale entries behind. This is
usually set in the user-level file, but a project-level value overrides it.

Unknown keys and unknown language names are reported as errors. Degenerate values
(`0` for `min_lines`, `min_occurrences`, `max_bucket`, `seed_window` or `max_file_bytes`,
or `max_file_bytes` above 4 GiB) are rejected too. Hidden directories are skipped by
default, matching ripgrep. A missing `dup-detector.toml` is fine and simply uses the
defaults.

## CLI

```bash
# scan a path (defaults to the current directory)
dup-detector scan <path>

# restrict languages and tune thresholds
dup-detector scan <path> --lang rust --lang python --min-lines 7 --min-occurrences 2 --max-groups 50

# JSON output (for tooling)
dup-detector scan <path> --json

# allow consistent literal renames to match
dup-detector scan <path> --parameterize-literals

# also scan files excluded by .gitignore
dup-detector scan <path> --no-ignore

# raise the size limit for very large generated files
dup-detector scan <path> --max-file-bytes 8388608
```

Scan options:

| Flag                      | Default | Description                                  |
| ------------------------- | ------- | -------------------------------------------- |
| `--min-lines <N>`         | `7`     | Minimum line count for a clone group         |
| `--min-occurrences <N>`   | `2`     | Minimum occurrences per group                |
| `--max-groups <N>`        | none    | Maximum number of groups to report           |
| `--parameterize-literals` | off     | Treat consistently renamed literals as equal |
| `--lang <LANG>`           | all     | Restrict to a language (repeatable)          |
| `--no-ignore`             | off     | Also scan files excluded by `.gitignore`/`.ignore` |
| `--max-file-bytes <N>`    | `2097152` | Skip files larger than `N` bytes                 |
| `--json`                  | off     | Print results as JSON                        |

See [MCP server](mcp.md) for the `mcp` subcommand and [Editor integration](lsp.md)
for the `lsp` subcommand.
