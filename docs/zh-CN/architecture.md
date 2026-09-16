# 架构与实现

`dup-detector` 不是文本/正则匹配器。它用 tree-sitter 将源码解析为 token 流，并在
token 流上检测 **Type-1** 与 **Type-2** 克隆，使用**参数化（重命名不变）编码**。

## 处理流程

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

## 关键思想

1. **只取叶子 token。** 从 CST 抽取 token 会自然去除注释与格式差异，并统一多语言处理。
2. **参数化编码。** 标识符编码为“与上一次出现的距离”（首次出现为 0）。这样
   `a=b+c` 与 `x=y+z` 编码相同，同时仍要求结构一致。
3. **seed-and-extend。** 定长窗口哈希后分桶；过于通用的桶（出现次数过多）会被丢弃。
   候选对通过一一对应的**双射校验**后再最大化扩展。
4. **固定 token 精确匹配**；标识符走双射；字面量默认精确匹配，除非开启
   `parameterize_literals`。
5. **后处理。** 仅当文件偏移对齐保持不变时才合并重叠匹配；出现位置只按单一语义单元
   粒度报告（多单元匹配会被拆分）；过滤样板代码（导入、纯声明）；按大小排序克隆组。

完整规范与编码规范见 `AGENTS.md`。

## 项目结构

```
src/
  main.rs       CLI 入口：`mcp` / `lsp` / `scan`
  lib.rs        库根
  config.rs     Config + dup-detector.toml 读取
  language.rs   扩展名 -> LanguageId -> tree-sitter 语法
  model.rs      Token / SourceFile / Occurrence / CloneGroup / CloneType
  tokenize.rs   源码 -> CST -> 叶子 token 流
  encode.rs     token 流 -> 参数化编码
  detect.rs     seed-and-extend、双射校验、聚类
  index.rs      文件发现、并行解析、增量刷新
  cache.rs      磁盘 token 缓存（mtime/大小/文本哈希校验）
  server.rs     rmcp 服务器 + MCP 工具定义
  lsp.rs        LSP 服务器（诊断、跳转定义、引用、悬停）
editors/
  vscode/       VS Code 扩展（vscode-languageclient，打包为 .vsix）
  zed/          Zed 扩展（wasm，注册语言服务器名）
tests/
  corpus.rs     语料回归（重命名 / 常量 / 增行 / ……）
  corpus/       手写样例，含 precision/recall 断言
```

## 开发

```bash
cargo build                          # 日常构建
cargo build --release                # 性能敏感场景
cargo test                           # 单元测试 + 语料测试
cargo fmt                            # 格式化
cargo clippy --all-targets -- -D warnings
```
