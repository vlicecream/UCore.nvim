# UCore.nvim

Unreal Engine Neovim Library

[English](#english) | [中文](#中文)

---

## English

UCore.nvim is a Neovim plugin for Unreal Engine C++ development. It uses a Rust backend (`u_scanner` + `u_core_server`) to index source files, modules, assets, config, symbols, and references — then exposes everything through Neovim commands, keymaps, pickers, and completion sources.

> Status: actively developed. Core workflows are stable.

### Features

- **Smart Navigation**: `gd` (definition), `gD` (declaration), `gi` (implementation), `gr` (references), `gs` (source/header toggle), `gf` (global fuzzy find)
- **One-command boot**: `:UCore` starts the server and indexes your project automatically
- **Unreal Editor integration**: `:UCore build` builds with live log streaming; `:UCore editor` launches the editor
- **High-performance Rust index**: SQLite-backed database with tree-sitter C++ parsing for fast symbol lookup across large projects
- **Auto-pairs**: nvim-autopairs integration with UE-macro-aware rules (UFUNCTION, UPROPERTY)
- **blink.cmp completion source**: Context-aware Unreal-aware completions
- **Unreal C++ highlighting companion**: use `UTreeSitter.nvim` for parser registration, queries, and highlight activation
- **VCS Dashboard**: Perforce integration with pending changelists, checkout, commit
- **Project Dashboard**: `:UCore` opens a smart project console with live state badges

### Requirements

- Neovim 0.10+
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- `UTreeSitter.nvim` (optional, for Unreal C++ highlighting)
- `telescope.nvim` or `fzf-lua` (optional, for richer picker UI)
- `blink.cmp` (optional, for UCore completion source)
- `p4` command-line client (optional, for Perforce VCS features)

Windows is the primary development target.

### Installation

#### lazy.nvim

```lua
return {
  {
    "vlicecream/UCore.nvim",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "windwp/nvim-autopairs",
        event = "InsertEnter",
        opts = {},
      },
      {
        "vlicecream/UTreeSitter.nvim",
        lazy = false,
        dependencies = { "nvim-treesitter/nvim-treesitter" },
        opts = {},
      },
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
        ui = { picker = "telescope" },
        completion = { enable = true, keymap = "<C-l>" },
      })
    end,
  },
}
```

#### Local development

```lua
return {
  {
    dir = "C:/Unreal-NVIM/UCore.nvim",
    name = "UCore.nvim",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    config = function()
      require("ucore").setup({ auto_boot = true, ui = { picker = "telescope" } })
    end,
  },
}
```

### Quick Start

Open any file inside an Unreal project and run:

```vim
:UCore
```

This opens the **Dashboard** — a smart project console. Select `Boot current project` to start the server and index your project. With `auto_boot = true`, this happens automatically when you open a file.

### Keymaps

Default buffer-local navigation keymaps for Unreal C++ files:

| Key | Action | Description |
|-----|--------|-------------|
| `gd` | Definition | Jump to definition (header preferred) |
| `gD` | Declaration | Jump specifically to header declaration |
| `gi` | Implementation | Jump to .cpp implementation |
| `gr` | References | Find all usages in the project |
| `gs` | Source toggle | Switch between .h and .cpp |
| `gf` | Global find | Fuzzy search symbols, modules, assets, config |

All keymaps are configurable or can be disabled:

```lua
require("ucore").setup({
  navigation = {
    keymaps = {
      enable = true,
      definition = "gd",
      declaration = "gD",
      implementation = "gi",
      references = "gr",
      source_toggle = "gs",
      global_find = "gf",
    },
  },
})
```

### Commands

```vim
:UCore                          " Open project Dashboard
:UCore boot                     " Boot current project or pick a registered one
:UCore build [config] [plat]    " Build Unreal Editor target with live logs
:UCore build-cancel             " Cancel running build
:UCore editor                   " Build then launch Unreal Editor
:UCore globalfind [pattern]     " Fuzzy find symbols, modules, assets, config
:UCore goto <subcommand>        " Navigation subcommands
```

`:UCore goto` subcommands:

```vim
:UCore goto definition          " Go to definition (gd)
:UCore goto declaration         " Go to declaration (gD)
:UCore goto implementation      " Go to implementation (gi)
:UCore goto references          " Find references (gr)
:UCore goto source              " Toggle .h/.cpp (gs)
:UCore goto help                " Show subcommand help
```

VCS commands:

```vim
:UCore vcs                      " Open VCS Dashboard
:UCore vcs dashboard            " Same as above
:UCore vcs checkout             " p4 edit current file
:UCore vcs commit               " Open visual commit UI
:UCore vcs changelists          " View pending P4 changelists
:UCore changes                  " Legacy alias
:UCore checkout                 " Legacy alias
:UCore commit                   " Legacy alias
:UCore changelists              " Legacy alias
```

Diagnostics:

```vim
:UCore status                   " Open readable runtime status report
:UCore logs                     " Open the latest server log
:checkhealth ucore              " Environment diagnostics
```

### Configuration

```lua
require("ucore").setup({
  auto_boot = true,             -- Auto-boot on entering an Unreal project
  port = 30110,                 -- RPC server port
  use_release_binary = true,    -- Prefer release builds over cargo run
  ui = {
    picker = "auto",            -- "auto", "telescope", "fzf-lua", or "vim"
  },
  navigation = {
    keymaps = {
      enable = true,
      definition = "gd",
      declaration = "gD",
      implementation = "gi",
      references = "gr",
      source_toggle = "gs",
      global_find = "gf",
    },
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
    min_chars = 2,
    debounce_ms = 180,
  },
  build = {
    open_quickfix_on_error = true,
    include_warnings = true,
    color_log = true,
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
  diagnostics = {
    enable = true,
    underline = true,
    virtual_text = false,
    signs = true,
    debounce_ms = 300,
  },
})
```

### Architecture

UCore.nvim has two layers:

```
Neovim (Lua)
  ├── CLI bridge (u_scanner)    — lifecycle: setup, refresh, watch
  └── TCP + MsgPack RPC (u_core_server) — interactive: goto, references, completions
         │
         └── SQLite database     — indexed symbols, classes, members, modules, assets
```

The Rust backend parses Unreal C++ source files with a custom tree-sitter grammar (`unreal_cpp`), extracts classes, members, inheritance, includes, and calls — then stores everything in SQLite. The Lua frontend queries this database through the RPC server or CLI bridge.

Two backend binaries:

```text
u_scanner       CLI tool for setup, refresh, watch, and queries
u_core_server   Long-running TCP server for interactive features (port 30110)
```

### Rust Backend Build

The build script handles MSVC toolchain detection, stale object cleanup, and automatic server restart:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1
```

If the first release build times out in lazy.nvim (default 120s), raise the timeout:

```lua
require("lazy").setup(spec, {
  git = { timeout = 600 },
})
```

Manual build:

```powershell
cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner
```

### Runtime Data

```
stdpath("data")/ucore/registry.json
stdpath("data")/ucore/server-registry.json
stdpath("data")/ucore/projects/<name>-<hash>/ucore.db
stdpath("data")/ucore/projects/<name>-<hash>/ucore-cache.db
stdpath("data")/ucore/projects/<name>-<hash>/u_core_server.log
stdpath("data")/ucore/engines/<id>/engine.db
```

Use `:UCore status` to inspect current paths.

### Troubleshooting

```vim
:checkhealth ucore     " Environment check
:UCore status           " Runtime state
:UCore logs             " Server logs
```

Common fixes:

- **Rust missing**: Install from `https://rustup.rs/`
- **Server offline**: Run `:UCore`
- **Database missing**: Run `:UCore` inside the project and wait for indexing
- **Engine DB missing**: Keep Neovim open until background Engine index finishes
- **Build fails with MinGW errors**: Run `scripts/build.ps1 -Clean` to purge stale objects
- **Keymap conflicts**: Disable individual keymaps by setting them to `false` in config

### License

MIT

---

## 中文

UCore.nvim 是一个面向 Unreal Engine C++ 开发的 Neovim 插件。Rust 后端索引源码、模块、资产、配置、符号和引用，Lua 前端通过命令、快捷键、选择器和补全源暴露这些索引。

> 状态：积极开发中，核心工作流稳定。

### 特性

- **智能导航**：`gd`（定义）、`gD`（声明）、`gi`（实现）、`gr`（引用）、`gs`（.h/.cpp 切换）、`gf`（全局搜索）
- **一键启动**：`:UCore` 自动启动服务器并索引项目
- **Unreal Editor 集成**：`:UCore build` 构建并实时显示日志；`:UCore editor` 构建后启动编辑器
- **高性能 Rust 索引**：SQLite 数据库 + tree-sitter C++ 解析，大项目秒级符号查找
- **blink.cmp 补全源**：Unreal 感知的上下文补全
- **Unreal C++ 高亮 companion**：使用 `UTreeSitter.nvim` 负责 parser 注册、queries 和高亮启动
- **VCS Dashboard**：Perforce 集成，支持 pending changelist、checkout、commit
- **项目仪表盘**：`:UCore` 打开带实时状态徽章的项目控制台

### 依赖

- Neovim 0.10+
- Rust 工具链（含 `cargo`）
- 含 `.uproject` 的 Unreal Engine 项目
- `UTreeSitter.nvim`（可选，Unreal C++ 高亮需要）
- `telescope.nvim` 或 `fzf-lua`（可选，更丰富的选择器 UI）
- `blink.cmp`（可选，UCore 补全源）
- `p4` 命令行客户端（可选，Perforce VCS 功能）

Windows 是主要开发目标平台。

### 安装

#### lazy.nvim

```lua
return {
  {
    "vlicecream/UCore.nvim",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "windwp/nvim-autopairs",
        event = "InsertEnter",
        opts = {},
      },
      {
        "vlicecream/UTreeSitter.nvim",
        lazy = false,
        dependencies = { "nvim-treesitter/nvim-treesitter" },
        opts = {},
      },
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
        ui = { picker = "telescope" },
        completion = { enable = true, keymap = "<C-l>" },
      })
    end,
  },
}
```

#### 本地开发

```lua
return {
  {
    dir = "C:/Unreal-NVIM/UCore.nvim",
    name = "UCore.nvim",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    config = function()
      require("ucore").setup({ auto_boot = true, ui = { picker = "telescope" } })
    end,
  },
}
```

### 快速开始

在 Unreal 项目中打开任意文件后运行：

```vim
:UCore
```

这将打开 **Dashboard**（仪表盘）。选择 `Boot current project` 启动服务器并索引项目。设置 `auto_boot = true` 后，打开文件时会自动启动。

### 快捷键

Unreal C++ 文件的默认 buffer-local 导航快捷键：

| 按键 | 功能 | 说明 |
|------|------|------|
| `gd` | 定义跳转 | 跳转到定义，优先 header |
| `gD` | 声明跳转 | 专门跳转到 header 声明 |
| `gi` | 实现跳转 | 跳转到 .cpp 实现 |
| `gr` | 查找引用 | 全工程搜索使用位置 |
| `gs` | 源文件切换 | .h 和 .cpp 间切换 |
| `gf` | 全局搜索 | 模糊搜索符号、模块、资产、配置 |

所有快捷键均可配置或关闭：

```lua
require("ucore").setup({
  navigation = {
    keymaps = {
      enable = true,
      definition = "gd",
      declaration = "gD",
      implementation = "gi",
      references = "gr",
      source_toggle = "gs",
      global_find = "gf",
    },
  },
})
```

### 命令

```vim
:UCore                          " 打开项目仪表盘
:UCore boot                     " 启动当前项目或选择已注册项目
:UCore build [配置] [平台]      " 构建 Editor 目标，实时日志
:UCore build-cancel             " 取消正在运行的构建
:UCore editor                   " 构建后启动 Unreal Editor
:UCore globalfind [pattern]     " 模糊搜索符号、模块、资产、配置
:UCore goto <subcommand>        " 导航子命令
```

`:UCore goto` 子命令：

```vim
:UCore goto definition          " 跳转定义 (gd)
:UCore goto declaration         " 跳转声明 (gD)
:UCore goto implementation      " 跳转实现 (gi)
:UCore goto references          " 查找引用 (gr)
:UCore goto source              " 切换 .h/.cpp (gs)
:UCore goto help                " 显示子命令帮助
```

VCS 命令：

```vim
:UCore vcs                      " 打开 VCS Dashboard
:UCore vcs dashboard            " 同上
:UCore vcs checkout             " p4 edit 当前文件
:UCore vcs commit               " 打开可视化提交界面
:UCore vcs changelists          " 查看 P4 pending changelist
```

诊断：

```vim
:UCore status                   " 查看运行时状态
:UCore logs                     " 打开最新服务器日志
:checkhealth ucore              " 环境诊断
```

### 配置

```lua
require("ucore").setup({
  auto_boot = true,             -- 进入 Unreal 项目时自动启动
  port = 30110,                 -- RPC 服务器端口
  use_release_binary = true,    -- 优先使用 release 构建
  ui = {
    picker = "auto",            -- "auto", "telescope", "fzf-lua", "vim"
  },
  navigation = {
    keymaps = {
      enable = true,
      definition = "gd",
      declaration = "gD",
      implementation = "gi",
      references = "gr",
      source_toggle = "gs",
      global_find = "gf",
    },
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
    min_chars = 2,
    debounce_ms = 180,
  },
  build = {
    open_quickfix_on_error = true,
    include_warnings = true,
    color_log = true,
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
  diagnostics = {
    enable = true,
    underline = true,
    virtual_text = false,
    signs = true,
    debounce_ms = 300,
  },
})
```

### 架构

```
Neovim (Lua)
  ├── CLI 桥接 (u_scanner)     — 生命周期：setup, refresh, watch
  └── TCP + MsgPack RPC (u_core_server) — 交互：goto, references, completions
         │
         └── SQLite 数据库     — 索引：符号、类、成员、模块、资产
```

Rust 后端使用自定义 tree-sitter 语法解析 Unreal C++ 源码，提取类、成员、继承、包含和调用信息，存入 SQLite。Lua 前端通过 RPC 或 CLI 查询该数据库。

两个后端二进制文件：

```text
u_scanner       设置、刷新、监听和查询的 CLI 工具
u_core_server   交互功能的长时间运行 TCP 服务器（端口 30110）
```

### 后端构建

构建脚本自动处理 MSVC 工具链检测、过时对象清理和服务器自动重启：

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1
```

手动构建：

```powershell
cargo build --release --manifest-path u-scanner/Cargo.toml --bin u_core_server --bin u_scanner
```

### 运行时数据

```
stdpath("data")/ucore/registry.json
stdpath("data")/ucore/server-registry.json
stdpath("data")/ucore/projects/<name>-<hash>/ucore.db
stdpath("data")/ucore/projects/<name>-<hash>/ucore-cache.db
stdpath("data")/ucore/projects/<name>-<hash>/u_core_server.log
stdpath("data")/ucore/engines/<id>/engine.db
```

使用 `:UCore status` 查看当前路径。

### 故障排除

```vim
:checkhealth ucore     " 环境检查
:UCore status           " 运行时状态
:UCore logs             " 服务器日志
```

常见问题：

- **缺少 Rust**：从 `https://rustup.rs/` 安装
- **服务器离线**：运行 `:UCore`
- **数据库缺失**：在项目中运行 `:UCore` 并等待索引
- **引擎 DB 缺失**：保持 Neovim 打开直到后台索引完成
- **MinGW 构建失败**：运行 `scripts/build.ps1 -Clean` 清理过时对象
- **快捷键冲突**：在配置中将对应快捷键设为 `false` 关闭

### 许可

MIT
