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
})
```

Recommended configuration:

```lua
require("ucore").setup({
  auto_boot = true,
  port = 30110,
})
```

`auto_boot = true` starts UCore automatically when opening a file inside an
Unreal project.

`auto_boot = true` 会在打开 Unreal 工程内文件时自动启动 UCore。

## Rust Backend

During development, UCore can run the Rust backend through `cargo run`.

开发阶段，UCore 可以通过 `cargo run` 启动 Rust 后端。

For faster startup, build release binaries manually:

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

更快的启动方式是先手动构建 release binary：

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

A future command will provide this directly:

```vim
:UCore build
```

后续会提供 `:UCore build` 直接完成这一步。

## Data Files

By default, UCore stores project-local data under:

```text
<YourUnrealProject>/.ucore/
```

Typical files:

```text
ucore.db
ucore-cache.db
u_core_server.log
registry.json
```

默认情况下，UCore 会把数据库和日志放在 Unreal 工程目录下的 `.ucore/`。

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
