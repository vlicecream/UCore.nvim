# UCore.nvim

Unreal Engine project index and workflow companion for Neovim.

[English](#english) | [中文](#中文)

---

## English

`UCore.nvim` is the project/index layer in the U-series stack.

It focuses on:

- Unreal project boot and registry
- Rust-backed indexing for symbols, modules, assets, and config
- definition / declaration / implementation / references navigation
- project explorer, global search, completion, diagnostics, semantic overlay
- shared bottom output tabs for build / debug / Unreal runtime streams

It does **not** own syntax highlighting or VCS anymore:

- highlighting lives in [`UTreeSitter.nvim`](https://github.com/vlicecream/UTreeSitter.nvim)
- version control lives in [`UVersionControlSystem.nvim`](https://github.com/vlicecream/UVersionControlSystem.nvim)

### Features

- `:UCore` smart entry for booting the current Unreal project
- `UScanner` Rust backend (`u_scanner` + `u_core_server`) with SQLite caches
- `gd` / `gD` / `gi` / `gr` / `gs` / `gf` navigation workflow
- explorer for `Project / Source / Config`
- `blink.cmp` completion source
- buffer diagnostics and semantic highlights from the UCore index
- `nvim-autopairs` integration for common Unreal C++ editing flow

### Requirements

- Neovim 0.10+
- Git
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- `telescope.nvim` or `fzf-lua` if you want richer picker UI
- `blink.cmp` if you want the UCore completion source
- `nvim-autopairs` if you want pair/newline integration

### Installation

#### UCore Only

```lua
return {
  {
    "vlicecream/UCore.nvim",
    main = "ucore",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "windwp/nvim-autopairs",
        event = "InsertEnter",
        opts = {},
      },

      {
        "saghen/blink.cmp",
        opts = function(_, opts)
          return require("ucore.completion.blink").extend_blink_opts(opts)
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
    opts = {
      auto_boot = true,
      explorer = {
        auto_open = false,
      },
      completion = {
        min_chars = 2,
        debounce_ms = 180,
      },
      ui = {
        picker = "telescope",
      },
    },
  },
}
```

Optional companion plugins:

- install `UTreeSitter.nvim` if you want Unreal tree-sitter highlighting
- install `UVersionControlSystem.nvim` if you want the Unreal VCS dashboard and actions
- install `UBuildTool.nvim` if you want Unreal build / editor launch
- install `UDebugTool.nvim` if you want Unreal debugging

`extend_blink_opts()` only prepares `blink.cmp` at config time. UCore does not patch blink at runtime.

During build, `UCore.nvim` resolves the backend like this:

1. clone `UScanner` into `stdpath("data")/ucore/backend/UScanner` when missing
2. update that managed checkout on later builds
3. compile `u_scanner` and `u_core_server` there

### Semantic Diagnostics

UCore owns the editing workflow for Unreal C++:

- indexed completion
- indexed navigation / references / global find
- Unreal-specific diagnostics and fixes
- build-output diagnostics rendering

UCore itself focuses on:

- Unreal-specific diagnostics
- smart `Alt+Enter` fallback fixes
- index-aware include insertion

Recommended setup:

```lua
{ "vlicecream/UCore.nvim" }
```

Final compiler truth should come from Unreal build output / MSVC, not from a separate external diagnostics layer.

### blink.cmp Keymaps

`extend_blink_opts()` also fills in a small default keymap only when you have not already set one:

- `<Tab>` selects the next completion item
- `<S-Tab>` selects the previous completion item
- `<CR>` accepts the selected completion item
- when the completion menu is not visible, mappings fall back to your normal key behavior

If you want your own `Tab` behavior, override it after calling `extend_blink_opts()`:

```lua
{
  "saghen/blink.cmp",
  opts = function(_, opts)
    opts = require("ucore.completion.blink").extend_blink_opts(opts)

    opts.keymap["<Tab>"] = {
      function(cmp)
        if cmp.is_menu_visible() then
          return cmp.accept()
        end
        if cmp.snippet_active() then
          return cmp.snippet_forward()
        end
      end,
      "fallback",
    }

    opts.keymap["<S-Tab>"] = {
      function(cmp)
        if cmp.snippet_active() then
          return cmp.snippet_backward()
        end
      end,
      "fallback",
    }

    opts.keymap["<CR>"] = { "fallback" }

    return opts
  end,
}
```

### Quick Start

Open any file inside an Unreal project and run:

```vim
:UCore
```

With `auto_boot = true`, UCore boots automatically when you enter the project.

### Output Workspace

When companion plugins emit runtime output into UCore, a bottom tabbed workspace opens automatically.

- newest tabs are inserted at the front and shown immediately
- build logs, debug session updates, adapter install progress, and Unreal launch messages can share the same area
- inside the workspace:
  - `<Tab>` / `<S-Tab>` or `H` / `L` switch tabs
  - `q` closes the current tab
  - `x` closes the workspace

### Commands

```vim
:UCore
:UCore boot
:UCore explorer        " toggle the explorer
:UCore find [pattern]
:UCore goto <definition|declaration|implementation|references|source>
:UCore help
:checkhealth ucore
```

### Default Keymaps

| Key | Action |
| --- | --- |
| `gd` | go to definition |
| `gD` | go to declaration |
| `gi` | go to implementation |
| `gr` | find references |
| `gs` | toggle `.h` / `.cpp` |
| `gf` | global find |

### Configuration

```lua
require("ucore").setup({
  auto_boot = true,
  explorer = {
    auto_open = false,
  },
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "telescope",
    output = {
      enable = true,
      auto_open = true,
      height = 12,
      max_tabs = 8,
    },
  },
  completion = {
    min_chars = 2,
    debounce_ms = 180,
  },
  diagnostics = {
    enable = true,
    action_keymap = "<leader>ca",
    underline = true,
    virtual_text = false,
    signs = true,
    update_in_insert = true,
    debounce_ms = 300,
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
  autopairs = {
    enable = true,
    map_cr = true,
    check_ts = true,
  },
})
```

### Architecture

```text
Neovim (Lua)
  ├── CLI bridge: u_scanner
  └── TCP + MsgPack RPC: u_core_server
          └── SQLite caches / project index
```

`UCore.nvim` prefers built binaries from the managed `UScanner` checkout under `stdpath("data")/ucore/backend/UScanner`. If no release binaries are available, it falls back to `cargo run` against that checkout.

### Split Responsibilities

`UCore.nvim` now focuses on index / navigation / completion / diagnostics.

Use the split repos for runtime workflows:

- `UScanner` for the Rust backend source (`u_scanner` and `u_core_server`)
- `UBuildTool.nvim` for Unreal build, build cancel, and editor launch
- `UDebugTool.nvim` for Unreal attach / launch-under-debugger / breakpoints / minimal debug UI

### Troubleshooting

```vim
:checkhealth ucore
:UCore
```

Common cases:

- Rust missing: install from `https://rustup.rs/`
- project not indexed yet: run `:UCore` and wait for boot/indexing
- server not ready: run `:UCore`, wait for boot, then re-run `:checkhealth ucore`
- no syntax highlight: install `UTreeSitter.nvim`, then run `:checkhealth utreesitter`

### Related Repositories

```text
UTreeSitter                  grammar + queries + parser tests
UTreeSitter.nvim             Neovim parser/filetype/highlight integration
UVersionControlSystem.nvim   Unreal VCS dashboard and actions
UBuildTool.nvim              Unreal build, editor launch
UDebugTool.nvim              Unreal debugging on top of nvim-dap
UCore.nvim                   Unreal project index, RPC, navigation, completion
```

### License

MIT

---

## 中文

`UCore.nvim` 是 U 系列里的项目索引和工作流层。

它主要负责：

- Unreal 项目启动和注册
- Rust 后端索引符号、模块、资产、配置
- 定义 / 声明 / 实现 / 引用跳转
- 项目浏览器、全局搜索、补全、诊断、语义高亮

它**不再**负责语法高亮和版本控制：

- 高亮由 [`UTreeSitter.nvim`](https://github.com/vlicecream/UTreeSitter.nvim) 负责
- 版本控制由 [`UVersionControlSystem.nvim`](https://github.com/vlicecream/UVersionControlSystem.nvim) 负责

### 特性

- `:UCore` 作为当前 Unreal 项目的智能入口
- `UScanner` Rust 后端（`u_scanner` + `u_core_server`）+ SQLite 缓存
- `gd` / `gD` / `gi` / `gr` / `gs` / `gf` 导航工作流
- `Project / Source / Config` 三栏浏览器
- `blink.cmp` 补全源
- 基于 UCore 索引的 buffer 诊断和语义高亮
- `nvim-autopairs` 的 Unreal C++ 编辑集成

### 依赖

- Neovim 0.10+
- Git
- Rust 工具链和 `cargo`
- 含 `.uproject` 的 Unreal Engine 项目
- `telescope.nvim` 或 `fzf-lua`（需要更完整的 picker 时）
- `blink.cmp`（需要 UCore 补全源时）
- `nvim-autopairs`（需要自动配对和回车展开时）

### 安装

#### 仅安装 UCore

```lua
return {
  {
    "vlicecream/UCore.nvim",
    main = "ucore",
    lazy = false,
    build = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1",
    dependencies = {
      {
        "windwp/nvim-autopairs",
        event = "InsertEnter",
        opts = {},
      },

      {
        "saghen/blink.cmp",
        opts = function(_, opts)
          return require("ucore.completion.blink").extend_blink_opts(opts)
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
    opts = {
      auto_boot = true,
      explorer = {
        auto_open = false,
      },
      completion = {
        min_chars = 2,
        debounce_ms = 180,
      },
      ui = {
        picker = "telescope",
      },
    },
  },
}
```

可选配套插件：

- 需要 Unreal tree-sitter 高亮时再装 `UTreeSitter.nvim`
- 需要 Unreal VCS 面板和操作时再装 `UVersionControlSystem.nvim`
- 需要 Unreal 构建、启动 Editor 时再装 `UBuildTool.nvim`
- 需要 Unreal 调试时再装 `UDebugTool.nvim`

`extend_blink_opts()` 只在配置阶段补全 `blink.cmp` 选项，UCore 不会在运行时改写 blink 配置。

构建时，`UCore.nvim` 会按这个顺序解析后端：

1. 缺失时自动把 `UScanner` 拉到 `stdpath("data")/ucore/backend/UScanner`
2. 后续构建自动更新这份托管 checkout
3. 并在那里面编译 `u_scanner` 和 `u_core_server`

### 语义诊断

UCore 直接接管 Unreal C++ 的编辑工作流：

- 基于索引的补全
- 基于索引的跳转 / 引用 / 全局搜索
- Unreal 专属诊断和修复
- build 输出诊断展示

UCore 自己主要负责：

- Unreal 专属规则诊断
- `Alt+Enter` 的智能回退修复
- 基于索引的 include 插入

推荐接法：

```lua
{ "vlicecream/UCore.nvim" }
```

最终编译真相请以 Unreal build / MSVC 为准，不再依赖额外的外部诊断层。

### blink.cmp 按键

`extend_blink_opts()` 还会补上一套很小的默认按键，但前提是你自己没有先写：

- `<Tab>` 选择下一个补全项
- `<S-Tab>` 选择上一个补全项
- `<CR>` 确认当前补全项
- 当补全菜单没打开时，这些按键会回退到你原本的按键行为

如果你想完全接管自己的 `Tab` 行为，可以在调用 `extend_blink_opts()` 之后覆盖：

```lua
{
  "saghen/blink.cmp",
  opts = function(_, opts)
    opts = require("ucore.completion.blink").extend_blink_opts(opts)

    opts.keymap["<Tab>"] = {
      function(cmp)
        if cmp.is_menu_visible() then
          return cmp.accept()
        end
        if cmp.snippet_active() then
          return cmp.snippet_forward()
        end
      end,
      "fallback",
    }

    opts.keymap["<S-Tab>"] = {
      function(cmp)
        if cmp.snippet_active() then
          return cmp.snippet_backward()
        end
      end,
      "fallback",
    }

    opts.keymap["<CR>"] = { "fallback" }

    return opts
  end,
}
```

### 快速开始

在 Unreal 项目里打开任意文件后运行：

```vim
:UCore
```

如果启用了 `auto_boot = true`，进入项目时会自动启动。

### 命令

```vim
:UCore
:UCore boot
:UCore explorer        " 切换 explorer
:UCore find [pattern]
:UCore goto <definition|declaration|implementation|references|source>
:UCore help
:checkhealth ucore
```

### 默认快捷键

| 按键 | 功能 |
| --- | --- |
| `gd` | 跳转定义 |
| `gD` | 跳转声明 |
| `gi` | 跳转实现 |
| `gr` | 查找引用 |
| `gs` | `.h` / `.cpp` 切换 |
| `gf` | 全局搜索 |

### 配置

```lua
require("ucore").setup({
  auto_boot = true,
  explorer = {
    auto_open = false,
  },
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "telescope",
  },
  completion = {
    min_chars = 2,
    debounce_ms = 180,
  },
  diagnostics = {
    enable = true,
    action_keymap = "<leader>ca",
    underline = true,
    virtual_text = false,
    signs = true,
    debounce_ms = 300,
  },
  semantic = {
    enable = true,
    debounce_ms = 120,
  },
  autopairs = {
    enable = true,
    map_cr = true,
    check_ts = true,
  },
})
```

### 架构

```text
Neovim (Lua)
  ├── CLI 桥接: u_scanner
  └── TCP + MsgPack RPC: u_core_server
          └── SQLite 缓存 / 项目索引
```

`UCore.nvim` 运行时优先使用托管 `UScanner` checkout 里已经构建好的二进制；若 release binary 不可用，则回退到这份源码树上执行 `cargo run`。

### 职责拆分

`UCore.nvim` 现在只负责索引 / 跳转 / 补全 / 诊断。

运行时工作流请使用拆分仓库：

- `UScanner`：负责 Rust 后端源码（`u_scanner` 和 `u_core_server`）
- `UBuildTool.nvim`：负责 Unreal 构建、停止构建、启动 Editor
- `UDebugTool.nvim`：负责 Unreal attach、调试器下启动、断点、最轻调试 UI

### 排查

```vim
:checkhealth ucore
:UCore
```

常见情况：

- 没装 Rust：从 `https://rustup.rs/` 安装
- 项目还没建索引：运行 `:UCore` 并等待 boot/index 完成
- 服务没有起来：先执行 `:UCore`，等待 boot 完成后再运行 `:checkhealth ucore`
- 没有语法高亮：安装 `UTreeSitter.nvim`，然后运行 `:checkhealth utreesitter`

### 相关仓库

```text
UTreeSitter                  grammar + queries + parser tests
UTreeSitter.nvim             Neovim parser/filetype/highlight integration
UVersionControlSystem.nvim   Unreal VCS dashboard and actions
UBuildTool.nvim              Unreal 构建、启动 Editor
UDebugTool.nvim              基于 nvim-dap 的 Unreal 调试
UCore.nvim                   Unreal project index, RPC, navigation, completion
```

### 许可

MIT
