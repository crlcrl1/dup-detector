# 编辑器集成（LSP）

以 `dup-detector lsp` 启动语言服务器，即可在编辑器中直接看到重复代码：

- **警告** —— 打开文件中每个克隆出现位置都会报告为 `Warning` 诊断，范围覆盖重复片段，
  消息中列出其他出现位置（可通过 related information 跳转）。
- **跳转到重复片段** —— 在克隆的任意位置执行 go to definition 会跳转到其他出现位置；
  find references 会列出全部位置。
- **悬停** —— 显示克隆大小、类型以及所有出现位置。

编辑器配置示例（Neovim）：

```lua
vim.lsp.start({
  name = "dup-detector",
  cmd = { "/absolute/path/to/dup-detector", "lsp" },
  root_dir = vim.fs.root(0, { "dup-detector.toml", ".git" }),
})
```

项目配置（`dup-detector.toml`）从工作区根目录读取（若不存在则回退到服务器启动目录）。

## 内置编辑器插件

可直接安装的插件位于 [`editors/`](../../editors/)：

- [`editors/vscode`](../../editors/vscode) —— VS Code 扩展（编译 LSP 客户端并打包为 `.vsix`）。
- [`editors/zed`](../../editors/zed) —— Zed 扩展。Zed 无法仅靠配置启动任意语言服务器
  （服务器名必须由扩展注册），因此该扩展负责注册 `dup-detector`。

## LSP 的效率设计

检测是项目级的，若每次按键都重新检测将非常浪费。服务器因此：

- 使用**增量文本同步**，只重新解析发生变化的文档；
- 对编辑进行**防抖**（350 ms）并合并连续变更，通过 generation 计数取消过期分析；
- 将 CPU 密集的分析放在**阻塞线程池**上执行，保持请求处理响应迅速；
- **每次编辑增量检测**：只重新检测被修改的 token 窗口，加上上一次报告过的克隆的窗口
  签名，因此未受影响的克隆会被精确复现，候选规模与改动量成正比而非仓库大小；
- 复用内存索引的 **mtime/大小增量刷新** 与每文件种子签名缓存；
- 每个文件最多发布 `100` 条诊断，避免刷屏。
