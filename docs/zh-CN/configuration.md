# 配置文件与 CLI

## 配置文件（`dup-detector.toml`）

同一文件名会从两个层级读取，并按配置项逐个合并：

- **用户级**：Linux 下为 `~/.config/dup-detector/dup-detector.toml`（设置了
  `XDG_CONFIG_HOME` 时为 `$XDG_CONFIG_HOME/dup-detector/dup-detector.toml`），macOS
  下为 `~/Library/Application Support/dup-detector/dup-detector.toml`，Windows 下为
  `%APPDATA%\dup-detector\dup-detector.toml`。对该机器上的所有项目生效。
- **项目级**：在项目根目录放置 `dup-detector.toml`。启动时从被扫描路径向上查找
  该文件（`scan`），MCP 服务器则从其工作目录以及每个 `scope` 根目录向上查找，因此扫描
  子目录时也能找到项目配置。

项目级文件中出现的键优先；其余键回退到用户级文件，最后回退到内置默认值。`mcp` 与
`lsp` 服务器会同时监视两个文件，任一发生变化时自动重载并重建受影响的索引。命令行参数
与 MCP 工具参数会覆盖两级配置文件。所有键均可选。

```toml
# ~/.config/dup-detector/dup-detector.toml（或 <project>/dup-detector.toml）
min_lines = 7
min_occurrences = 2
max_bucket = 32          # 大于该值的种子桶视为通用模式并丢弃
seed_window = 8          # 种子窗口的 token 长度
max_groups = 100         # 省略或注释掉表示不限制
parameterize_literals = false
languages = ["rust", "python", "javascript", "typescript", "tsx", "c", "cpp"]
no_ignore = false         # 同时扫描被 .gitignore/.ignore 排除的文件
include_hidden = false    # 同时扫描 .github 等点目录（默认关闭）
max_file_bytes = 2097152   # 超过 2 MiB 的文件跳过
filter_boilerplate = true  # 丢弃无逻辑标记的纯声明类克隆
cache_location = "project" # "project"（根目录下 .dup-detector/）或 "user-cache"
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
| `no_ignore`             | `false` | 同时扫描被 `.gitignore`/`.ignore` 排除的文件 |
| `include_hidden`        | `false` | 同时扫描隐藏（点）目录，如 `.github`       |
| `max_file_bytes`        | `2097152` | 超过该大小的文件跳过（限制解析内存）          |
| `filter_boilerplate`    | `true`  | 丢弃无逻辑标记的纯声明类克隆组             |
| `cache_location`        | `project` | 解析缓存的存放位置：`project` 或 `user-cache` |

`cache_location = "project"` 会把每个源文件的缓存条目放在 `<项目根>/.dup-detector/`；
`"user-cache"` 则改放到平台用户缓存目录中——Linux 为
`~/.cache/dup-detector/<根路径哈希>`，macOS 为 `~/Library/Caches/dup-detector/<哈希>`，
Windows 为 `%LOCALAPPDATA%\dup-detector\<哈希>`——从而保持项目目录干净。`reindex`
工具会同时清空两个位置，切换该设置后不会残留旧条目。该键通常写在用户级文件中，但
项目级取值会覆盖它。对 MCP 服务器而言，项目根固定为服务器的启动目录：`scope`
参数不影响缓存位置；`scan` CLI 和 LSP 服务器仍按扫描目录/工作区根目录存放。
缓存条目还记录了语法指纹（kind 名称、ABI 版本、插件 kind 覆盖表），因此升级
语法或修改插件 manifest 后，即使源文件本身没变，旧 token 缓存也会自动失效重解析。

## 语法插件

除了内置语言（rust、python、javascript、typescript、tsx、c、cpp）之外，任何
tree-sitter 语法都可以在运行时以插件方式接入，无需重新编译。最快的方式是一条
命令：

```bash
dup-detector install go            # 从 GitHub 安装 tree-sitter/tree-sitter-go
dup-detector install org/repo      # 任意其他语法仓库
dup-detector install go -e golang  # 需要时追加文件扩展名
dup-detector install go --force    # 覆盖已安装的同名插件
```

`install` 会浅克隆仓库、从语法的 `tree-sitter.json` 读取文件扩展名（或用
`-e/--extension` 指定）、用系统 C 编译器编译 `src/parser.c`（有的话加
`src/scanner.c`）并写入 manifest。依赖 `git` 和 C 编译器（`cc`，可用 `CC` 环境
变量覆盖）。装完用 `dup-detector languages` 验证，或运行 `dup-detector doctor`
检查工具链依赖并逐个复检已安装插件（有缺失或损坏时退出码非零）。

也可以手动安装：插件是用户级语法目录（Linux 为 `~/.config/dup-detector/grammars`，
macOS 为 `~/Library/Application Support/dup-detector/grammars`，Windows 为
`%APPDATA%\dup-detector\grammars`）下的一个目录，内含一个导出
`tree_sitter_<name>` 符号的共享库（`*.so` / `*.dll` / `*.dylib`）和一个
`grammar.toml` manifest：

```toml
name = "go"                    # 可选，默认为目录名
extensions = ["go"]            # 必填
library = "tree-sitter-go.so"  # 目录里只有一个共享库时可省略
symbol = "tree_sitter_go"      # 可选，默认为 tree_sitter_<name>
identifier_kinds = [...]       # 可选，追加式分类覆盖
literal_kinds = [...]
unit_kinds = [...]
container_kinds = [...]
```

token 分类基于 tree-sitter 节点 kind 名称的内置启发式（如 `identifier`、`string`、
`block`、`*_statement`），大多数语法可以直接套用；当某个语法的节点命名不惯例时，
可用 `*_kinds` 列表追加覆盖。放入插件后，`dup-detector languages` 会列出它，
`languages` 配置键和 `--lang` 参数也接受它的名字。损坏的插件只会记录日志并跳过，
不影响内置语言和其他插件。语法插件是加载进进程的原生代码：请只安装可信来源的
语法库。

未知的键和未知的语言名会报错。退化取值（`min_lines`、`min_occurrences`、`max_bucket`、
`seed_window`、`max_file_bytes` 为 `0`，或 `max_file_bytes` 超过 4 GiB）也会被拒绝。
默认跳过隐藏目录（与 ripgrep 行为一致）。缺少 `dup-detector.toml` 时直接使用默认值。

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

# 提高上限以包含超大生成文件
dup-detector scan <path> --max-file-bytes 8388608
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
| `--max-file-bytes <N>`    | `2097152` | 跳过大于 `N` 字节的文件                     |
| `--json`                  | 关闭   | 以 JSON 输出结果                 |

`mcp` 子命令见 [MCP 服务器](mcp.md)，`lsp` 子命令见[编辑器集成](lsp.md)。
