# Configuration and CLI

## Project configuration (`dup-detector.toml`)

Configuration is per project: put a `dup-detector.toml` at the project root. It
is read at startup by searching upwards from the scanned path for `scan`, and
from the server's working directory and each `scope` root for the MCP server, so
scanning a subdirectory still finds the project config. Command-line flags and
MCP tool parameters override the file. Every key is optional and falls back to
the default.

```toml
# dup-detector.toml
min_lines = 7
min_occurrences = 2
max_bucket = 32          # seed buckets larger than this are dropped as generic
seed_window = 8          # seed window length in tokens
max_groups = 100         # omit / comment out for no limit
parameterize_literals = false
languages = ["rust", "python", "javascript", "typescript", "tsx", "cpp"]
max_file_bytes = 2097152   # skip files larger than 2 MiB
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
| `max_file_bytes`        | `2097152` | Files larger than this are skipped (bounds parse memory)         |

Unknown keys and unknown language names are reported as errors. A missing
`dup-detector.toml` is fine and simply uses the defaults.

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
