# UCore.nvim

UCore.nvim is a Neovim plugin for Unreal Engine projects.

UCore.nvim 是一个面向 Unreal Engine 工程的 Neovim 插件。

It uses a Rust backend, `u-scanner`, to index Unreal C++ source files, modules,
assets, configs, and symbols. The Lua frontend then exposes those results inside
Neovim through commands, pickers, and RPC.

它使用 Rust 后端 `u-scanner` 索引 Unreal C++ 源码、模块、资产、配置和符号。
Lua 前端再通过命令、选择器和 RPC 在 Neovim 中使用这些数据。

## Status

This project is still early and evolving quickly.

当前项目还处于早期阶段，接口和功能仍在快速迭代。

## Requirements

- Neovim 0.10+
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- Windows is the current primary development target

依赖：

- Neovim 0.10+
- Rust 工具链和 `cargo`
- 一个带 `.uproject` 文件的 Unreal Engine 工程
- 当前主要开发目标是 Windows

## Installation

### lazy.nvim from GitHub

```lua
{
  "vlicecream/UCore.nvim",
  build = "cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner",
  config = function()
    require("ucore").setup({
      auto_boot = true,
    })
  end,
}
```

### lazy.nvim local development

```lua
{
  dir = "C:/Unreal-NVIM/UCore.nvim",
  name = "UCore.nvim",
  build = "cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner",
  config = function()
    require("ucore").setup({
      auto_boot = true,
    })
  end,
}
```

Use `dir` only for a real local path. Do not use `dir = "vlicecream/UCore.nvim"`.

`dir` 只能写真实本地路径，不要写 `dir = "vlicecream/UCore.nvim"`。

## Quick Start

Open a file inside your Unreal project, then run:

```vim
:UCore
```

This is the same as:

```vim
:UCore boot
```

It will:

1. Start the Rust server.
2. Register the current Unreal project.
3. Refresh the database if needed.
4. Start file watching.

打开 Unreal 工程里的任意文件，然后运行：

```vim
:UCore
```

它会自动启动 Rust server、注册工程、按需刷新索引，并启动文件监听。

## Commands

User-facing commands:

```vim
:UCore
:UCore boot
:UCore find
:UCore goto
:UCore references
:UCore help
```

Debug commands:

```vim
:UCore debug status
:UCore debug rpc-status
:UCore debug start
:UCore debug stop
:UCore debug restart
:UCore debug setup
:UCore debug refresh
:UCore debug maps
:UCore debug help
```

Health check:

```vim
:checkhealth ucore
```

## Configuration

Recommended configuration:

```lua
require("ucore").setup({
  auto_boot = true,
  port = 30110,
  use_release_binary = true,
  completion = {
    enable = true,
    keymap = "<C-l>",
    min_chars = 2,
    debounce_ms = 180,
  },
})
```

`auto_boot = true` starts UCore automatically when you open an Unreal project file.

`use_release_binary = true` prefers binaries built by lazy.nvim's `build` step, and
falls back to `cargo run` if release binaries are missing.

`completion.enable = true` only controls the manual insert-mode mapping.
Native automatic completion is always enabled.

`auto_boot = true` 会在打开 Unreal 工程文件时自动启动 UCore。

`use_release_binary = true` 会优先使用 lazy.nvim `build` 阶段构建的 release
binary；如果不存在，则回退到 `cargo run`。

`completion.enable = true` 只控制手动插入模式快捷键。原生自动补全始终开启。

### blink.cmp Integration

UCore can integrate into blink.cmp as a normal completion source.

```lua
{
  "saghen/blink.cmp",
  opts = {
    sources = {
      default = { "lsp", "path", "snippets", "buffer", "ucore" },
      providers = {
        ucore = {
          name = "UCore",
          module = "ucore.completion.blink",
          async = true,
          timeout_ms = 2000,
          min_keyword_length = 0,
          score_offset = 50,
        },
      },
    },
  },
}
```

## Rust Backend

During development, UCore can run the Rust backend through `cargo run`.
For faster startup, let lazy.nvim build release binaries during install/update.

```lua
{
  "vlicecream/UCore.nvim",
  build = "cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner",
}
```

You can also build manually:

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

也可以手动构建：

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

## Data Files

By default, UCore stores runtime data under Neovim's cache directory:

```text
stdpath("cache")/ucore/projects/<project-name>-<hash>/
```

Typical files:

```text
ucore.db
ucore-cache.db
u_core_server.log
registry.json
```

默认情况下，UCore 会把数据库和日志放在 Neovim cache 目录下，不会污染 Unreal 工程目录。

## Troubleshooting

Run:

```vim
:checkhealth ucore
```

If the server is not reachable:

```vim
:UCore
```

or debug manually:

```vim
:UCore debug start
:UCore debug rpc-status
```

If Rust is not installed, install it from:

```text
https://rustup.rs/
```

如果遇到问题，优先运行：

```vim
:checkhealth ucore
```

它会检查 Rust/Cargo、u-scanner、当前 Unreal 工程、数据库和 server 连接状态。

## Development Notes

The Lua frontend talks to the Rust backend in two ways:

1. CLI bridge through `u_scanner`.
2. Direct TCP + MessagePack RPC for interactive queries.

Lua 前端和 Rust 后端有两条通信路径：

1. 通过 `u_scanner` CLI 桥接。
2. 通过 TCP + MessagePack RPC 直连，用于交互式查询。

## License

MIT
