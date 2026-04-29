# UCore.nvim

[English](#english) | [中文](#中文)

---

## English

UCore.nvim is a Neovim plugin for Unreal Engine projects.

It uses a Rust backend, `u-scanner`, to index Unreal C++ source files, modules,
assets, config files, symbols, definitions, references, and project metadata.
The Lua frontend exposes that index inside Neovim through commands, Telescope
pickers, completion sources, semantic highlights, and health checks.

> Status: early and evolving. The core workflow is usable, but APIs may still
> change while the plugin matures.

### Features

- One-command Unreal project boot: `:UCore`
- Build the current Unreal Editor target with live logs: `:UCore build`
- Open the current project in Unreal Editor: `:UCore editor`
- Shared project and Unreal Engine indexes
- Find indexed code, modules, assets, and config values with `:UCore find`
- Go to definition with `:UCore goto`
- Find references with `:UCore references`
- Unreal C++ Tree-sitter parser registration and highlight defaults
- Optional Telescope picker integration
- Optional blink.cmp completion source
- Runtime status and logs with `:UCore status` and `:UCore logs`
- Environment diagnostics through `:checkhealth ucore`

### Requirements

- Neovim 0.10+
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- `nvim-treesitter` if you want Unreal C++ Tree-sitter highlighting
- `telescope.nvim` or `fzf-lua` if you want a richer picker UI
- `blink.cmp` if you want UCore as a completion source

Windows is the primary development target today.

### Installation

#### lazy.nvim

```lua
return {
  {
    "vlicecream/UCore.nvim",
    lazy = false,
    build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "saghen/blink.cmp",
        opts = function(_, opts)
          opts.sources = opts.sources or {}
          opts.sources.default = opts.sources.default or { "lsp", "path", "snippets", "buffer" }

          if not vim.tbl_contains(opts.sources.default, "ucore") then
            table.insert(opts.sources.default, "ucore")
          end

          opts.sources.providers = opts.sources.providers or {}
          opts.sources.providers.ucore = {
            name = "UCore",
            module = "ucore.completion.blink",
            async = true,
            timeout_ms = 2000,
            min_keyword_length = 0,
            score_offset = 50,
          }

          return opts
        end,
      },
      {
        "nvim-telescope/telescope.nvim",
        dependencies = {
          "nvim-lua/plenary.nvim",
          "nvim-tree/nvim-web-devicons",
        },
      },
    },
    config = function()
      require("ucore").setup({
        auto_boot = true,
        ui = {
          picker = "telescope",
        },
        completion = {
          enable = true,
          keymap = "<C-l>",
        },
      })
    end,
  },
  {
    "vlicecream/UTreeSitter",
    lazy = false,
  },
  {
    "nvim-treesitter/nvim-treesitter",
    lazy = false,
    build = ":TSUpdate",
    opts = function(_, opts)
      opts = opts or {}
      opts.auto_install = true
      opts.ensure_installed = opts.ensure_installed or {}

      if not vim.tbl_contains(opts.ensure_installed, "unreal_cpp") then
        table.insert(opts.ensure_installed, "unreal_cpp")
      end

      return opts
    end,
  },
}
```

#### Local development

Use `dir` only for a real local checkout:

```lua
return {
  {
    dir = "C:/Unreal-NVIM/UCore.nvim",
    name = "UCore.nvim",
    lazy = false,
    build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    config = function()
      require("ucore").setup({
        auto_boot = true,
        ui = {
          picker = "telescope",
        },
      })
    end,
  },
  {
    dir = "C:/Unreal-NVIM/UTreeSitter",
    name = "UTreeSitter",
    lazy = false,
  },
}
```

Do not use `dir = "vlicecream/UCore.nvim"` unless that is an actual local path.

### First Run

Open any file inside an Unreal project and run:

```vim
:UCore
```

This opens the **Dashboard** — a smart project console that shows your current
project, server status, and available actions. From the Dashboard you can boot,
find symbols, build, open the editor, or inspect runtime state.

When you select `Boot current project`, the full boot sequence runs:

1. Register the current Unreal project in UCore's global registry.
2. Resolve the project's Unreal Engine root from `EngineAssociation`.
3. Start the Rust server.
4. Create or reuse project databases under Neovim's data directory.
5. Refresh the project index if needed.
6. Start file watching.
7. Refresh the shared Unreal Engine index if needed.

With `auto_boot = true`, booting also happens automatically when opening files
in an Unreal project.

### Commands

User commands:

```vim
:UCore              " Open project Dashboard with live state per item
:UCore boot         " Boot current project, or choose a registered project
:UCore build        " Build <ProjectName>Editor Win64 Development with live logs
:UCore build-cancel " Cancel the currently running Unreal build
:UCore editor       " Build, then open the current .uproject in Unreal Editor
:UCore find         " Find symbols, modules, assets, and config entries
:UCore goto         " Go to definition at cursor
:UCore references   " Find references at cursor
:UCore status       " Open a readable UCore runtime status report
:UCore help         " Show user commands
```

The Dashboard (`:UCore`) shows state badges and descriptions on every item —
project name, index readiness (`ready` / `needs boot` / `no project`), server
status, and registered project count. Actions that need a project or an index
show a helpful redirect when the prerequisite is missing.

Diagnostics:

```vim
:checkhealth ucore
```

Debug commands (internal lifecycle, logs, and diagnostics):

```vim
:UCore debug help
:UCore debug logs         " Open the latest UCore server log
:UCore debug status
:UCore debug rpc-status
:UCore debug start
:UCore debug stop
:UCore debug restart
:UCore debug setup
:UCore debug refresh
:UCore debug register
:UCore debug open
:UCore debug projects
:UCore debug engine
:UCore debug engine-refresh
:UCore debug modules
:UCore debug assets
:UCore debug maps
:UCore debug complete
```

### Configuration

Default configuration:

```lua
require("ucore").setup({
  auto_boot = true,
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "auto", -- "auto", "telescope", "fzf-lua", or "vim"
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
    min_chars = 2,
    debounce_ms = 180,
  },
  build = {
    open_quickfix_on_error = true,  -- auto-open quickfix when build has errors
    include_warnings = true,         -- include warnings in the quickfix list
    color_log = true,                -- colorize build log with extmarks
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
})
```

`use_release_binary = true` prefers binaries built by lazy.nvim's `build` step.
If release binaries are missing, UCore falls back to `cargo run`.

### Rust Backend

lazy.nvim can build the Rust backend automatically:

```lua
build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1"
```

On Windows, the build script loads the MSVC C++ toolchain before building C
dependencies such as SQLite and tree-sitter. It also retries once after
`cargo clean` if it detects stale MinGW/GCC-built objects.

Manual build:

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

The two backend binaries are:

```text
u_scanner       CLI bridge for setup, refresh, watch, and queries
u_core_server   long-running RPC server used by interactive features
```

### Unreal Build and Editor

Build the current project's default editor target:

```vim
:UCore build
```

Cancel a running build:

```vim
:UCore build-cancel
```

By default this runs:

```text
<EngineRoot>/Engine/Build/BatchFiles/Build.bat <ProjectName>Editor Win64 Development -Project="<Project.uproject>" -WaitMutex
```

The build output is streamed live into a Neovim log buffer with color-coded
lines — errors in red, warnings in yellow, success in green, commands in cyan.

On completion, diagnostics are parsed from the build output and populate the
quickfix list. If the build has errors, the quickfix window opens automatically
so you can jump directly to the failing line.

```
:copen          " Open the quickfix window
:cn             " Jump to next error
:cp             " Jump to previous error
```

Optional arguments:

```vim
:UCore build Development Win64
:UCore build DebugGame Win64
:UCore build Development Win64 MyProjectEditor
```

Build, then open the current project in Unreal Editor:

```vim
:UCore editor
```

UCore first runs the same default editor target build. If the build succeeds, it
launches `UnrealEditor.exe` or `UE4Editor.exe` with the current `.uproject`. If
the build fails, UCore keeps the build log open and does not launch the editor.

### Tree-sitter

UCore registers a custom `unreal_cpp` parser backed by:

```text
https://github.com/vlicecream/UTreeSitter
```

`require("ucore").setup()` registers:

- the `unreal_cpp` parser config
- Unreal project filetype detection for `.cpp`, `.h`, `.hpp`, `.inl`, etc.
- default highlight groups for Unreal C++

When you open an Unreal C++ file inside a project for the first time:

1. UCore detects the file belongs to an Unreal project and sets filetype `unreal_cpp`.
2. It checks whether the parser can attach. If not, it waits for nvim-treesitter
   and the parser config to become ready (retries up to 20 times, 300ms apart).
3. Once ready, it runs `:TSInstallSync unreal_cpp` automatically.
4. After installation, highlighting starts on the current buffer — no reopen needed.

If the install fails or the network is slow, UCore retries silently and only shows
a single warning after all retries are exhausted:

```
:checkhealth ucore
:TSInstallSync unreal_cpp
```

No restart or repeated file opens needed — the first open Just Works.

### Completion

Manual completion is available through the configured insert-mode keymap:

```lua
completion = {
  enable = true,
  keymap = "<C-l>",
}
```

blink.cmp integration is provided by:

```lua
module = "ucore.completion.blink"
```

### Runtime Data

UCore stores runtime data under Neovim's data directory. It does not write
databases into your Unreal project by default.

Typical paths:

```text
stdpath("data")/ucore/registry.json
stdpath("data")/ucore/server-registry.json
stdpath("data")/ucore/projects/<project-name>-<hash>/ucore.db
stdpath("data")/ucore/projects/<project-name>-<hash>/ucore-cache.db
stdpath("data")/ucore/projects/<project-name>-<hash>/u_core_server.log
stdpath("data")/ucore/engines/<engine-id>/engine.db
stdpath("data")/ucore/engines/<engine-id>/engine-cache.db
```

Use this to inspect the current paths:

```vim
:UCore status
```

### Troubleshooting

Start with:

```vim
:checkhealth ucore
```

Then inspect runtime state:

```vim
:UCore status
```

Then inspect backend logs:

```vim
:UCore logs
```

Common fixes:

- If Rust is missing, install it from `https://rustup.rs/`.
- If the server is offline, run `:UCore`.
- If the project database is missing, run `:UCore` inside the project and wait for indexing.
- If the Engine database is missing, keep Neovim open until the background Engine index finishes.
- If `unreal_cpp` is unsupported, load UCore first, then run `:TSInstall unreal_cpp`.
- If Windows release build fails with `__mingw_vfprintf` or `___chkstk_ms`, stale C objects were probably built with MinGW/GCC and then linked with MSVC. Use the bundled build script, or clean the backend once and rebuild:

```powershell
cd path\to\UCore.nvim
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1 -Clean

# Or manually:
cd u-scanner
cargo clean
cargo build --release --bin u_core_server --bin u_scanner
```

After that, run `:Lazy build UCore.nvim` again if you installed through lazy.nvim.

### Development Notes

The Lua frontend talks to the Rust backend in two ways:

1. CLI bridge through `u_scanner`.
2. TCP + MessagePack RPC through `u_core_server`.

Interactive features prefer RPC. CLI is still used for lifecycle and fallback
paths.

### License

MIT

---

## 中文

UCore.nvim 是一个面向 Unreal Engine 项目的 Neovim 插件。

它使用 Rust 后端 `u-scanner` 索引 Unreal C++ 源码、模块、资产、配置文件、符号、定义、
引用和项目元数据。Lua 前端通过命令、Telescope 选择器、补全源、语义高亮和健康检查
在 Neovim 中暴露该索引。

> 状态：早期开发中。核心工作流可用，但 API 可能随插件成熟而变化。

### 特性

- 一键启动 Unreal 项目：`:UCore`
- 构建当前 Editor 目标，实时显示日志：`:UCore build`
- 在 Unreal Editor 中打开当前项目：`:UCore editor`
- 共享项目和引擎索引
- 用 `:UCore find` 查找代码、模块、资产和配置
- 用 `:UCore goto` 跳转到定义
- 用 `:UCore references` 查找引用
- 注册 Unreal C++ Tree-sitter 解析器和默认高亮
- 可选的 Telescope 选择器集成
- 可选的 blink.cmp 补全源
- 运行时状态和日志：`:UCore status` / `:UCore logs`
- 环境诊断：`:checkhealth ucore`

### 依赖

- Neovim 0.10+
- Rust 工具链（含 `cargo`）
- 一个含 `.uproject` 文件的 Unreal Engine 项目
- `nvim-treesitter`（需要 Unreal C++ Tree-sitter 高亮时安装）
- `telescope.nvim` 或 `fzf-lua`（需要更丰富的选择器 UI 时安装）
- `blink.cmp`（需要 UCore 补全源时安装）

目前主要开发目标为 Windows 平台。

### 安装

#### lazy.nvim

```lua
return {
  {
    "vlicecream/UCore.nvim",
    lazy = false,
    build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "saghen/blink.cmp",
        opts = function(_, opts)
          opts.sources = opts.sources or {}
          opts.sources.default = opts.sources.default or { "lsp", "path", "snippets", "buffer" }

          if not vim.tbl_contains(opts.sources.default, "ucore") then
            table.insert(opts.sources.default, "ucore")
          end

          opts.sources.providers = opts.sources.providers or {}
          opts.sources.providers.ucore = {
            name = "UCore",
            module = "ucore.completion.blink",
            async = true,
            timeout_ms = 2000,
            min_keyword_length = 0,
            score_offset = 50,
          }

          return opts
        end,
      },
      {
        "nvim-telescope/telescope.nvim",
        dependencies = {
          "nvim-lua/plenary.nvim",
          "nvim-tree/nvim-web-devicons",
        },
      },
    },
    config = function()
      require("ucore").setup({
        auto_boot = true,
        ui = {
          picker = "telescope",
        },
        completion = {
          enable = true,
          keymap = "<C-l>",
        },
      })
    end,
  },
  {
    "vlicecream/UTreeSitter",
    lazy = false,
  },
  {
    "nvim-treesitter/nvim-treesitter",
    lazy = false,
    build = ":TSUpdate",
    opts = function(_, opts)
      opts = opts or {}
      opts.auto_install = true
      opts.ensure_installed = opts.ensure_installed or {}

      if not vim.tbl_contains(opts.ensure_installed, "unreal_cpp") then
        table.insert(opts.ensure_installed, "unreal_cpp")
      end

      return opts
    end,
  },
}
```

#### 本地开发

仅在实际本地检出的情况下使用 `dir`：

```lua
return {
  {
    dir = "C:/Unreal-NVIM/UCore.nvim",
    name = "UCore.nvim",
    lazy = false,
    build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    config = function()
      require("ucore").setup({
        auto_boot = true,
        ui = {
          picker = "telescope",
        },
      })
    end,
  },
  {
    dir = "C:/Unreal-NVIM/UTreeSitter",
    name = "UTreeSitter",
    lazy = false,
  },
}
```

不要使用 `dir = "vlicecream/UCore.nvim"`，除非那是一个真实的本地路径。

### 首次运行

在 Unreal 工程中打开任意文件，然后运行：

```vim
:UCore
```

这将打开 **Dashboard**（仪表盘）—— 一个智能项目控制台，显示当前项目、服务器状态和可用操作。
通过 Dashboard 你可以启动、查找符号、构建项目、打开编辑器或检查运行时状态。

选择 `Boot current project` 后，完整的启动序列如下：

1. 将当前 Unreal 项目注册到 UCore 全局注册表。
2. 从 `EngineAssociation` 解析项目的引擎根目录。
3. 启动 Rust 服务器。
4. 在 Neovim data 目录下创建或复用项目数据库。
5. 按需刷新项目索引。
6. 启动文件监听。
7. 按需刷新共享的引擎索引。

设置 `auto_boot = true` 后，在 Unreal 工程中打开文件时会自动启动。

### 命令

用户命令：

```vim
:UCore              " 打开项目仪表盘
:UCore boot         " 启动当前项目或选择已注册项目
:UCore build        " 构建并实时显示日志
:UCore build-cancel " 取消当前正在运行的构建
:UCore editor       " 构建后打开 Unreal Editor
:UCore find         " 查找符号、模块、资产和配置
:UCore goto         " 跳转到光标处的定义
:UCore references   " 查找光标处的引用
:UCore status       " 查看运行时状态报告
:UCore help         " 显示用户命令
```

Dashboard (`:UCore`) 在每个项目上显示状态徽章和描述 —— 项目名称、索引就绪状态
（`ready` / `needs boot` / `no project`）、服务器状态和已注册项目数。
当缺少前置条件时，需要项目或索引的操作会显示帮助性跳转提示。

诊断：

```vim
:checkhealth ucore
```

调试命令（生命周期、日志和诊断）：

```vim
:UCore debug help
:UCore debug logs         " 打开最新 UCore 服务器日志
:UCore debug status
:UCore debug rpc-status
:UCore debug start
:UCore debug stop
:UCore debug restart
:UCore debug setup
:UCore debug refresh
:UCore debug register
:UCore debug open
:UCore debug projects
:UCore debug engine
:UCore debug engine-refresh
:UCore debug modules
:UCore debug assets
:UCore debug maps
:UCore debug complete
```

### 配置

默认配置：

```lua
require("ucore").setup({
  auto_boot = true,
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "auto", -- "auto", "telescope", "fzf-lua", or "vim"
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
    min_chars = 2,
    debounce_ms = 180,
  },
  build = {
    open_quickfix_on_error = true,  -- 构建出错时自动打开 quickfix
    include_warnings = true,         -- 在 quickfix 中包含警告
    color_log = true,                -- 用 extmarks 给构建日志着色
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
})
```

`use_release_binary = true` 优先使用 lazy.nvim `build` 步骤编译的二进制文件。
如果 release 二进制文件缺失，UCore 回退到 `cargo run`。

### Rust 后端

lazy.nvim 可以自动构建 Rust 后端：

```lua
build = "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1"
```

在 Windows 上，构建脚本会在编译 SQLite、tree-sitter 等 C 依赖之前加载 MSVC C++ 工具链。
如果检测到过时的 MinGW/GCC 构建的对象，会自动执行 `cargo clean` 并重试一次。

手动构建：

```powershell
cd u-scanner
cargo build --release --bin u_core_server --bin u_scanner
```

两个后端二进制文件：

```text
u_scanner       CLI 桥接：设置、刷新、监听和查询
u_core_server   长运行 RPC 服务器，用于交互功能
```

### Unreal 构建和编辑器

构建当前项目的默认 Editor 目标：

```vim
:UCore build
```

取消正在运行的构建：

```vim
:UCore build-cancel
```

默认执行：

```text
<EngineRoot>/Engine/Build/BatchFiles/Build.bat <ProjectName>Editor Win64 Development -Project="<Project.uproject>" -WaitMutex
```

构建输出实时流式写入 Neovim 日志缓冲区，带有颜色编码 —— 错误红色、警告黄色、成功绿色、命令青色。

完成后，从构建输出解析诊断信息并填充 quickfix 列表。如果构建有错误，quickfix 窗口会自动打开。

```
:copen          " 打开 quickfix 窗口
:cn             " 跳转到下一个错误
:cp             " 跳转到上一个错误
```

可选参数：

```vim
:UCore build Development Win64
:UCore build DebugGame Win64
:UCore build Development Win64 MyProjectEditor
```

构建后打开 Unreal Editor：

```vim
:UCore editor
```

UCore 首先运行默认 Editor 目标构建。如果构建成功，使用当前 `.uproject` 启动
`UnrealEditor.exe` 或 `UE4Editor.exe`。如果构建失败，UCore 保持构建日志打开，
不启动编辑器。

### Tree-sitter

UCore 注册了自定义的 `unreal_cpp` 解析器，基于：

```text
https://github.com/vlicecream/UTreeSitter
```

`require("ucore").setup()` 注册：

- `unreal_cpp` 解析器配置
- Unreal 工程文件类型检测（`.cpp`, `.h`, `.hpp`, `.inl` 等）
- Unreal C++ 默认高亮组

首次在工程中打开 Unreal C++ 文件时：

1. UCore 检测到文件属于 Unreal 工程，设置文件类型为 `unreal_cpp`。
2. 检查解析器是否可附加。如果不可用，等待 nvim-treesitter 和解析器配置就绪（最多重试 20 次，间隔 300ms）。
3. 就绪后自动运行 `:TSInstallSync unreal_cpp`。
4. 安装后高亮立即生效，无需重新打开。

如果安装失败或网络较慢，UCore 静默重试，仅在所有重试耗尽后显示一条警告：

```
:checkhealth ucore
:TSInstallSync unreal_cpp
```

无需重启或重复打开文件 —— 首次打开即可正常工作。

### 补全

通过配置的插入模式快捷键触发手动补全：

```lua
completion = {
  enable = true,
  keymap = "<C-l>",
}
```

blink.cmp 集成由以下模块提供：

```lua
module = "ucore.completion.blink"
```

### 运行时数据

UCore 在 Neovim data 目录下存储运行时数据，默认不会写入你的 Unreal 项目。

典型路径：

```text
stdpath("data")/ucore/registry.json
stdpath("data")/ucore/server-registry.json
stdpath("data")/ucore/projects/<project-name>-<hash>/ucore.db
stdpath("data")/ucore/projects/<project-name>-<hash>/ucore-cache.db
stdpath("data")/ucore/projects/<project-name>-<hash>/u_core_server.log
stdpath("data")/ucore/engines/<engine-id>/engine.db
stdpath("data")/ucore/engines/<engine-id>/engine-cache.db
```

使用以下命令查看当前路径：

```vim
:UCore status
```

### 故障排除

从以下命令开始：

```vim
:checkhealth ucore
```

然后检查运行时状态：

```vim
:UCore status
```

然后检查后端日志：

```vim
:UCore logs
```

常见修复：

- 如果缺少 Rust，从 `https://rustup.rs/` 安装。
- 如果服务器离线，运行 `:UCore`。
- 如果项目数据库缺失，在项目中运行 `:UCore` 并等待索引。
- 如果引擎数据库缺失，保持 Neovim 打开直到后台引擎索引完成。
- 如果 `unreal_cpp` 不支持，先加载 UCore，再运行 `:TSInstall unreal_cpp`。
- 如果 Windows release 构建失败报 `__mingw_vfprintf` 或 `___chkstk_ms`，可能是过时的 C 对象由 MinGW/GCC 构建后用 MSVC 链接所致。使用内置构建脚本，或清理后端后重新构建：

```powershell
cd path\to\UCore.nvim
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1 -Clean

# 或手动执行：
cd u-scanner
cargo clean
cargo build --release --bin u_core_server --bin u_scanner
```

之后如果通过 lazy.nvim 安装，请再次运行 `:Lazy build UCore.nvim`。

### 开发说明

Lua 前端通过两种方式与 Rust 后端通信：

1. 通过 `u_scanner` 的 CLI 桥接。
2. 通过 `u_core_server` 的 TCP + MessagePack RPC。

交互功能优先使用 RPC。CLI 仍然用于生命周期和回退路径。

### 许可

MIT
