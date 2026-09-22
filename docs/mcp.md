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
the user-level config, falling back to the server's startup directory. Parsed
token streams are also cached, one entry per source file named by a hash of its
root-relative path (validated by mtime, size and a text hash), so new processes
only reparse changed files and only changed entries are rewritten. By default
the cache lives in `.dup-detector/` at the scanned root; `cache_location =
"user-cache"` moves it under the platform user cache directory instead (see
[Configuration](configuration.md)). Use `reindex` to clear it, and add
`.dup-detector/` to `.gitignore` if you keep the default location and don't want
it tracked. All logs go to stderr so the stdio protocol stays clean.

## Tools

| Tool                     | Purpose                               | Parameters                                                                                    |
| ------------------------ | ------------------------------------- | --------------------------------------------------------------------------------------------- |
| `find_clones`            | Project-wide duplicated code          | `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`, `parameterize_literals?` |
| `find_clones_in_file`    | Clones involving a given file         | `file`, `scope?`, `min_lines?`, `min_occurrences?`, `max_groups?`, `types?`                   |
| `find_clones_for_region` | "Is the code I'm writing duplicated?" | `file`, `start_line`, `end_line`, `scope?`, `min_lines?`, `max_groups?`, `types?`             |
| `reindex`                | Drop the in-memory index and clear its on-disk cache (rebuilt on next scan) | `path?`                                                                                       |

- `scope` defaults to the current working directory.
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
