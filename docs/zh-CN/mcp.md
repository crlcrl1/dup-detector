# MCP 服务器

`dup-detector` 可通过 stdio 作为常驻的 [MCP](https://modelcontextprotocol.io) 服务器运行：

```bash
dup-detector mcp
```

在 MCP 客户端中注册 release 产物。配置示例：

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

服务器按工作区根目录维护内存索引，并依据文件 mtime/大小进行增量刷新。每个根目录的
`dup-detector.toml` 与用户级配置合并后提供其配置，缺失时回退到服务器启动目录的配置。
解析后的 token 流还会被缓存，每个源文件一个条目、条目名为其相对根目录路径的哈希（通过
mtime、大小和文本哈希校验），因此新进程只需重新解析发生变化的文件，且只重写变化的条目。
默认缓存到扫描根目录下的 `.dup-detector/`；设置 `cache_location = "user-cache"` 可将其
改放到平台用户缓存目录（见[配置文件](configuration.md)）。可用 `reindex` 清空缓存。若
保留默认位置且不希望它被纳入版本控制，请将 `.dup-detector/` 加入 `.gitignore`。所有日志
都写入 stderr，以保持 stdio 协议干净。

## 工具

| 工具                     | 用途                           | 参数                                                                                          |
| ------------------------ | ------------------------------ | --------------------------------------------------------------------------------------------- |
| `find_clones`            | 全项目范围的重复代码           | `scope?`、`min_lines?`、`min_occurrences?`、`max_groups?`、`types?`、`parameterize_literals?` |
| `find_clones_in_file`    | 涉及指定文件的克隆             | `file`、`scope?`、`min_lines?`、`min_occurrences?`、`max_groups?`、`types?`                   |
| `find_clones_for_region` | “我正在写的这段代码是否重复？” | `file`、`start_line`、`end_line`、`scope?`、`min_lines?`、`max_groups?`、`types?`             |
| `reindex`                | 丢弃内存索引并清空磁盘缓存（下次扫描时重建） | `path?`                                                                                       |

- `scope` 默认为当前工作目录。
- `types` 接受 `"type-1"`、`"type-2"`。
- `find_clones_for_region` 的 `min_lines` 默认为 `min(config.min_lines, 查询区间的行数)`；若文件尚未被索引，则即时解析。`file` 必须解析到 `scope` 内的路径，通过 `..` 等逃逸的路径会被拒绝。

## 响应结构

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

响应保持精简：仅路径、行号范围、大小与克隆类型，不返回源码片段。
