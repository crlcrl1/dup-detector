# dup-detector

[English](README.md) | 简体中文

一个 [MCP](https://modelcontextprotocol.io) 服务器（同时提供 CLI），用于在代码库中查找**重复代码**，目标是与 JetBrains IDE 的 _Duplicated Code_ 检查对齐。

与基于文本/正则的方案不同，`dup-detector` 基于 **tree-sitter 生成的 token 流**，并使用**参数化（重命名不变）编码**，因此即使变量或字面量被重命名，只要结构相同也能识别出来。

## 克隆类型

| 类型   | 含义                                     | 支持情况           |
| ------ | ---------------------------------------- | ------------------ |
| Type-1 | 除空白/注释/格式外完全相同               | 支持               |
| Type-2 | 标识符（以及可选的字面量）被一致地重命名 | 支持（核心目标）   |

## 主要特性

- **语义级而非文本级** —— 注释、空白和格式完全不影响结果。
- **重命名不变** —— `a = b + c` 与 `x = y + z` 视为同一克隆。
- **多语言** —— Rust、Python、JavaScript、TypeScript/TSX、C++。
- **高性能** —— 并行解析（`rayon`）、遵循 `.gitignore` 的文件发现（`ignore`）、哈希种子分桶、基于 mtime 增量刷新的内存索引，以及跨进程复用的磁盘 token 缓存。
- **响应精简** —— 仅返回文件路径 + 行号范围 + 令牌数 + 克隆类型，不返回源码片段。
- **两种使用方式** —— 面向编码代理的常驻 MCP 服务器，以及 `scan` 命令行。

## 工作原理

```
文件发现（遵循 .gitignore）
  -> tree-sitter 解析         （仅取叶子 token：标识符、字面量、固定 token）
  -> 参数化编码               （标识符按“与上一次出现的距离”编码）
  -> 种子哈希                 （定长 token 窗口，默认 8）
  -> 双射校验                 （标识符必须一一对应）
  -> 最大化匹配扩展
  -> 聚类/合并/排序           （去重、过滤、按 token 数排序）
  -> 结果
```

关键思想（完整规范见 `AGENTS.md`）：

1. **只取叶子 token。** 从 CST 抽取 token 会自然去除注释与格式差异，并统一多语言处理。
2. **参数化编码。** 标识符编码为“与上一次出现的距离”（首次出现为 0）。这样 `a=b+c` 与 `x=y+z` 编码相同，同时仍要求结构一致。
3. **seed-and-extend。** 定长窗口哈希后分桶；过于通用的桶（出现次数过多）会被丢弃。候选对通过一一对应的**双射校验**后再最大化扩展。
4. **固定 token 精确匹配**；标识符走双射；字面量默认精确匹配，除非开启 `parameterize_literals`。

## 支持的语言

Rust、Python、JavaScript、TypeScript、TSX、C++（`.rs`、`.py`/`.pyi`、`.js`/`.mjs`/`.cjs`/`.jsx`、`.ts`/`.mts`/`.cts`、`.tsx`、`.cpp`/`.cc`/`.cxx`/`.c++`/`.hpp`/`.hh`/`.hxx`/`.h++`/`.h`/`.ipp`/`.inl`/`.tpp`）。

## 构建

需要 Rust 1.98+（edition 2024）。

```bash
cargo build --release
# 产物：target/release/dup-detector
```

## CLI 用法

```bash
# 通过 stdio 启动 MCP 服务器
dup-detector mcp

# 扫描某个路径（默认当前目录）
dup-detector scan <path>

# 限定语言并调整阈值
dup-detector scan <path> --lang rust --lang python --min-lines 4 --min-occurrences 2 --max-groups 50

# JSON 输出（便于工具集成）
dup-detector scan <path> --json

# 允许一致重命名的字面量参与匹配
dup-detector scan <path> --parameterize-literals
```

扫描参数：

| 参数                      | 默认值 | 说明                         |
| ------------------------- | ------ | ---------------------------- |
| `--min-lines <N>`         | `4`    | 克隆组的最小行数             |
| `--min-occurrences <N>`   | `2`    | 每个克隆组的最小出现次数     |
| `--max-groups <N>`        | 无限制 | 最多返回的克隆组数量         |
| `--parameterize-literals` | 关闭   | 将一致重命名的字面量视为相同 |
| `--lang <LANG>`           | 全部   | 限定语言（可重复）           |
| `--json`                  | 关闭   | 以 JSON 输出结果             |

## 作为 MCP 服务器使用

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

服务器按工作区根目录维护内存索引，并依据文件 mtime/大小进行增量刷新。解析后的 token 流还会缓存到扫描根目录下的 `.dup-detector/` 目录中，每个源文件一个条目、条目名为其相对根目录路径的哈希（通过 mtime、大小和文本哈希校验），因此新进程只需重新解析发生变化的文件，且只重写变化的条目；可用 `reindex` 清空该目录。如不希望它被纳入版本控制，请将 `.dup-detector/` 加入 `.gitignore`。所有日志都写入 stderr，以保持 stdio 协议干净。

### 工具

| 工具                     | 用途                           | 参数                                                                                                     |
| ------------------------ | ------------------------------ | -------------------------------------------------------------------------------------------------------- |
| `find_clones`            | 全项目范围的重复代码           | `scope?`、`min_lines?`、`min_occurrences?`、`max_groups?`、`types?`、`parameterize_literals?` |
| `find_clones_in_file`    | 涉及指定文件的克隆             | `file`、`scope?`、`min_lines?`、`min_occurrences?`、`max_groups?`、`types?`                             |
| `find_clones_for_region` | “我正在写的这段代码是否重复？” | `file`、`start_line`、`end_line`、`scope?`、`min_lines?`、`max_groups?`、`types?`                       |
| `reindex`                | 重建内存索引                   | `path?`                                                                                                  |

- `scope` 默认为当前工作目录。
- `types` 接受 `"type-1"`、`"type-2"`。
- `find_clones_for_region` 的 `min_lines` 默认取查询区间的行数；若文件尚未被索引，则即时解析。

### 响应结构

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

## 项目结构

```
src/
  main.rs       CLI 入口：`mcp` / `scan`
  lib.rs        库根
  config.rs     Config（阈值、语言开关）
  language.rs   扩展名 -> LanguageId -> tree-sitter 语法
  model.rs      Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs   源码 -> CST -> 叶子 token 流
  encode.rs     token 流 -> 参数化编码
  detect.rs     seed-and-extend、双射校验、聚类
  index.rs      文件发现、并行解析、增量刷新
  cache.rs      磁盘 token 缓存（mtime/大小/文本哈希校验）
  server.rs     rmcp 服务器 + MCP 工具定义
tests/
  corpus.rs     语料回归（重命名 / 常量 / 增行 / ……）
  corpus/       手写样例，含 precision/recall 断言
```

## 开发

```bash
cargo build
cargo test          # 单元测试 + 语料测试
cargo fmt           # 格式化
cargo clippy --all-targets -- -D warnings
```

`AGENTS.md` 详细记录了架构、算法与编码规范。

## 路线图

- Phase 0–6 已完成：token 提取、编码、检测、Type-1/2、基于 mtime 刷新的内存索引 + 磁盘 token 缓存、语料回归。

## 许可证

基于 [Apache License 2.0](LICENSE) 授权。
