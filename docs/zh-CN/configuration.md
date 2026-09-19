# 配置文件与 CLI

## 项目配置（`dup-detector.toml`）

配置以项目为单位：在项目根目录放置 `dup-detector.toml`。启动时从被扫描路径向上查找
该文件（`scan`），MCP 服务器则从其工作目录以及每个 `scope` 根目录向上查找，因此扫描
子目录时也能找到项目配置。命令行参数与 MCP 工具参数会覆盖配置文件。所有键均可选，
缺省时使用默认值。

```toml
# dup-detector.toml
min_lines = 7
min_occurrences = 2
max_bucket = 32          # 大于该值的种子桶视为通用模式并丢弃
seed_window = 8          # 种子窗口的 token 长度
max_groups = 100         # 省略或注释掉表示不限制
parameterize_literals = false
languages = ["rust", "python", "javascript", "typescript", "tsx", "cpp"]
```

| 键                      | 默认值  | 说明                                       |
| ----------------------- | ------- | ------------------------------------------ |
| `min_lines`             | `7`     | 克隆组的最小行数                           |
| `min_occurrences`       | `2`     | 每个克隆组的最小出现次数                   |
| `max_bucket`            | `32`    | 超过该大小的种子桶视为通用模式并丢弃       |
| `seed_window`           | `8`     | 种子窗口的 token 长度                      |
| `max_groups`            | 无限制  | 最多返回的克隆组数量                       |
| `parameterize_literals` | `false` | 将一致重命名的字面量视为相同               |
| `languages`             | 全部    | 参与扫描的语言（`LanguageId::from_name` 名称） |

未知的键和未知的语言名会报错。缺少 `dup-detector.toml` 时直接使用默认值。

## CLI

```bash
# 扫描某个路径（默认当前目录）
dup-detector scan <path>

# 限定语言并调整阈值
dup-detector scan <path> --lang rust --lang python --min-lines 7 --min-occurrences 2 --max-groups 50

# JSON 输出（便于工具集成）
dup-detector scan <path> --json

# 允许一致重命名的字面量参与匹配
dup-detector scan <path> --parameterize-literals

# 同时扫描被 .gitignore 排除的文件
dup-detector scan <path> --no-ignore
```

扫描参数：

| 参数                      | 默认值 | 说明                             |
| ------------------------- | ------ | -------------------------------- |
| `--min-lines <N>`         | `7`    | 克隆组的最小行数                 |
| `--min-occurrences <N>`   | `2`    | 每个克隆组的最小出现次数         |
| `--max-groups <N>`        | 无限制 | 最多返回的克隆组数量             |
| `--parameterize-literals` | 关闭   | 将一致重命名的字面量视为相同     |
| `--lang <LANG>`           | 全部   | 限定语言（可重复）               |
| `--no-ignore`             | 关闭   | 同时扫描被 `.gitignore`/`.ignore` 排除的文件 |
| `--json`                  | 关闭   | 以 JSON 输出结果                 |

`mcp` 子命令见 [MCP 服务器](mcp.md)，`lsp` 子命令见[编辑器集成](lsp.md)。
