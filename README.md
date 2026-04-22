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
:UCore modules
:UCore assets
:UCore search-symbols <pattern>
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

Default configuration:

```lua
require("ucore").setup({
  auto_boot = false,
  port = 30110,
  use_release_binary = true,
  completion = {
    enable = true,
    keymap = "<C-l>",
    auto_trigger = false,
    min_chars = 2,
    debounce_ms = 180,
  },
})
```

Recommended configuration:

```lua
require("ucore").setup({
  auto_boot = true,
  port = 30110,
  use_release_binary = true,
  completion = {
    enable = true,
    keymap = "<C-l>",
    auto_trigger = false,
    min_chars = 2,
    debounce_ms = 180,
  },
})
```

`auto_boot = true` starts UCore automatically when opening a file inside an
Unreal project.

`auto_boot = true` 会在打开 Unreal 工程内文件时自动启动 UCore。

`use_release_binary = true` makes UCore prefer binaries built by lazy.nvim's
`build` step. If release binaries do not exist, UCore falls back to `cargo run`.

`use_release_binary = true` 会让 UCore 优先使用 lazy.nvim `build` 阶段构建出的
release binary。如果 release binary 不存在，UCore 会回退到 `cargo run`。

`completion.keymap` controls the insert-mode key used to trigger manual
completion. Set `completion.enable = false` if you prefer to define your own
mapping.

`completion.keymap` 控制插入模式下触发手动补全的快捷键。如果你想自己管理映射，
可以设置 `completion.enable = false`。

`completion.auto_trigger = true` enables native Vim automatic completion while
typing. If you use blink.cmp, keep this disabled and register UCore as a blink
source instead.

`completion.auto_trigger = true` 会在输入时触发 Vim 原生补全。如果你使用
blink.cmp，建议保持关闭，并把 UCore 注册为 blink source。

### blink.cmp Integration

UCore can integrate into blink.cmp as a normal completion source, so candidates
show up in the same menu as LSP/buffer/snippet items.

UCore 可以作为普通补全源接入 blink.cmp，这样候选会显示在你现有的
LSP/buffer/snippet 同一个补全菜单里。

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

开发阶段，UCore 可以通过 `cargo run` 启动 Rust 后端。

For faster startup, let lazy.nvim build release binaries during install/update:

```lua
{
  "vlicecream/UCore.nvim",
  build = "cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner",
}
```

为了获得更快的启动速度，推荐让 lazy.nvim 在安装/更新插件时构建 release binary：

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
