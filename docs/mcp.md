# MCP server

`dup-detector` runs as a long-running [MCP](https://modelcontextprotocol.io)
server over stdio:

```bash
dup-detector mcp
```

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

The server keeps an in-memory index per workspace root and refreshes it
incrementally by file mtime/size. Each root merges its `dup-detector.toml` with
the user-level config, falling back to the server's startup directory. Scopes
are restricted to the startup directory and its subdirectories: a `scope`
outside the working directory is rejected, and no cache or index directories
are ever created outside it. Parsed token streams are also cached, one entry
per source file named by a hash of its root-relative path (validated by mtime,
size and a text hash), so new processes only reparse changed files and only
changed entries are rewritten. Cache entries are keyed by paths relative to
that startup directory, so indexing different scopes of one workspace reuses
the same entry per file instead of duplicating it. The cache always lives at
the startup directory
— in `.dup-detector/` there by default; `cache_location = "user-cache"` moves
it under the platform user cache directory instead (see
[Configuration](configuration.md)). Use `reindex` to clear it, and add
`.dup-detector/` to `.gitignore` if you keep the default location and don't want
it tracked. All logs go to stderr so the stdio protocol stays clean.

## Tools

| Tool                     | Purpose                               | Parameters                                                                                    |
| ------------------------ | ------------------------------------- | --------------------------------------------------------------------------------------------- |
| `find_clones`            | Project-wide duplicated code          | `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file`    | Clones involving a given file         | `file`, `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`                   |
| `find_clones_for_region` | "Is the code I'm writing duplicated?" | `file`, `start_line`, `end_line`, `scope?`, `min_lines?`, `max_groups?`, `types?`             |
| `reindex`                | Drop all in-memory indexes and clear the working directory's on-disk cache (rebuilt on next scan) | —                                                                                             |

- `scope` defaults to the current working directory; it must be the working directory or one of its subdirectories.
- `types` accepts `"type-1"`, `"type-2"`.
- `find_clones_for_region` defaults `min_lines` to `min(config.min_lines, line span of the queried
  region)` and parses the file on the fly if it is not indexed yet. `file` must resolve to a path
  inside `scope`; paths escaping it (e.g. via `..`) are rejected.

## Response shape

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

Responses are token-efficient: paths, line ranges, size and clone type, with no
source excerpts.

## Suggested global instruction for coding agents

Agents only benefit from the tools if they are told to use them. Add a block
like the one below to your agent's global instructions (e.g. `AGENTS.md` or
`CLAUDE.md`) so it reaches for dup-detector at the right moments:

```markdown
## Duplicate code detection (dup-detector MCP)

The `dup-detector` MCP server is available (tools `find_clones`,
`find_clones_in_file`, `find_clones_for_region`, `reindex`). Call it
proactively — don't wait to be asked:

- After adding or refactoring a substantial chunk of code, check the touched
  region with `find_clones_for_region` and tell the user about any duplication,
  suggesting what to extract.
- When asked to find duplicated code, assess refactoring opportunities, or
  clean up redundancy, scan the project with `find_clones` first.
- Before extracting a shared helper, use `find_clones` to locate every matching
  or near-matching implementation.
- If results look stale after a large batch of changes, call `reindex` first.

Responses only contain paths and line ranges (no source excerpts); open the
files yourself when you need the code.
```
