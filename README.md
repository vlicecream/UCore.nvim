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
- build + Unreal Editor launch
- project explorer, global search, completion, diagnostics, semantic overlay

It does **not** own syntax highlighting or VCS anymore:

- highlighting lives in [`UTreeSitter.nvim`](https://github.com/vlicecream/UTreeSitter.nvim)
- version control lives in [`UVersionControlSystem.nvim`](https://github.com/vlicecream/UVersionControlSystem.nvim)

### Features

- `:UCore` smart entry for booting the current Unreal project
- Rust backend (`u_scanner` + `u_core_server`) with SQLite caches
- `gd` / `gD` / `gi` / `gr` / `gs` / `gf` navigation workflow
- Unreal build integration with live log streaming
- Unreal Editor launch from Neovim
- explorer for `Project / Source / Config`
- `blink.cmp` completion source
- buffer diagnostics and semantic highlights from the UCore index
- `nvim-autopairs` integration for common Unreal C++ editing flow

### Requirements

- Neovim 0.10+
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- `telescope.nvim` or `fzf-lua` if you want richer picker UI
- `blink.cmp` if you want the UCore completion source
- `nvim-autopairs` if you want pair/newline integration
- `nvim-lspconfig` + `clangd` if you want semantic red/yellow diagnostics and LSP quick fixes

### Installation

#### Recommended Stack

```lua
return {
  {
    "vlicecream/UTreeSitter.nvim",
    main = "utreesitter",
    lazy = false,
    dependencies = {
      {
        "nvim-treesitter/nvim-treesitter",
        build = ":TSUpdate",
        opts = function(_, opts)
          opts = opts or {}
          opts.auto_install = true
          opts.indent = { enable = true }
          return opts
        end,
      },
    },
    opts = {},
  },

  {
    "vlicecream/UVersionControlSystem.nvim",
    main = "uvcs",
    lazy = false,
    opts = {
      enable = true,
      prompt_on_readonly_save = true,
      provider = "auto",
      p4 = {
        command = "p4",
        -- port = "127.0.0.1:1666",
        -- user = "YourUser",
        -- client = "YourWorkspace",
      },
    },
  },

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

      -- Optional on Neovim 0.11+, but still useful on older setups.
      { "neovim/nvim-lspconfig" },

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
      completion = {
        enable = true,
        keymap = "<C-l>",
      },
      ui = {
        picker = "telescope",
      },
    },
  },
}
```

`UTreeSitter.nvim` and `UVersionControlSystem.nvim` are separate top-level plugins. `UCore.nvim` no longer bundles either layer.
`extend_blink_opts()` only prepares `blink.cmp` at config time. UCore does not patch blink at runtime.

### Semantic Diagnostics

For real C++ red/yellow diagnostics, use `clangd`.

UCore itself focuses on:

- Unreal-specific diagnostics
- smart `Alt+Enter` fallback fixes
- index-aware include insertion

Undefined symbols, type mismatches, overload resolution, template errors, and most semantic diagnostics should come from `clangd`.

Recommended setup:

```lua
{
  "neovim/nvim-lspconfig", -- optional on Neovim 0.11+
}
```

`setup_clangd()` is Unreal-aware:

- adds `unreal_cpp` to clangd filetypes
- forwards blink capabilities when available
- prefers the Visual Studio LLVM `x64/bin/clangd.exe` on Windows when it is available
- auto-detects a nearby `compile_commands.json` when possible
- stages the active `compile_commands.json` into the UCore project cache and points clangd there
- by default, skips attaching clangd when no compilation database is found, to avoid noisy false diagnostics
- defaults to a low-memory clangd profile for Unreal projects; background indexing stays opt-in

For a normal Windows Unreal setup, you usually do not need to hardcode a `clangd.exe` path or manage `compile_commands.json` manually. UCore tries to resolve both automatically.

You can also generate the Unreal compilation database directly from UCore:

```vim
:UCore debug generate-db
```

If your compilation database lives outside the project root:

```lua
require("ucore").setup({
  lsp = {
    clangd = {
      compile_commands_dir = "D:/YourProject/.vscode",
    },
  },
})
```

If you explicitly want clangd to attach without `compile_commands.json`:

```lua
require("ucore").setup({
  lsp = {
    clangd = {
      require_compile_commands = false,
    },
  },
})
```

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

### Commands

```vim
:UCore
:UCore boot
:UCore build [configuration] [platform] [target]
:UCore build-cancel
:UCore editor
:UCore explorer
:UCore globalfind [pattern]
:UCore goto <definition|declaration|implementation|references|source>
:UCore debug status
:UCore debug logs
:UCore debug rpc-status
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
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "telescope",
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
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
  lsp = {
    clangd = {
      command = "clangd",
      compile_commands_dir = nil,
    },
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

The backend prefers release binaries under `u-scanner/target/release/` and falls back to `cargo run` when needed.

### Troubleshooting

```vim
:checkhealth ucore
:UCore debug status
:UCore debug logs
```

Common cases:

- Rust missing: install from `https://rustup.rs/`
- project not indexed yet: run `:UCore` and wait for boot/indexing
- server not ready: check `:UCore debug logs`
- no syntax highlight: install `UTreeSitter.nvim`, then run `:checkhealth utreesitter`

### Related Repositories

```text
UTreeSitter                  grammar + queries + parser tests
UTreeSitter.nvim             Neovim parser/filetype/highlight integration
UVersionControlSystem.nvim   Unreal VCS dashboard and actions
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
- 构建和 Unreal Editor 启动
- 项目浏览器、全局搜索、补全、诊断、语义高亮

它**不再**负责语法高亮和版本控制：

- 高亮由 [`UTreeSitter.nvim`](https://github.com/vlicecream/UTreeSitter.nvim) 负责
- 版本控制由 [`UVersionControlSystem.nvim`](https://github.com/vlicecream/UVersionControlSystem.nvim) 负责

### 特性

- `:UCore` 作为当前 Unreal 项目的智能入口
- Rust 后端（`u_scanner` + `u_core_server`）+ SQLite 缓存
- `gd` / `gD` / `gi` / `gr` / `gs` / `gf` 导航工作流
- Unreal 构建集成，实时日志输出
- 从 Neovim 内直接启动 Unreal Editor
- `Project / Source / Config` 三栏浏览器
- `blink.cmp` 补全源
- 基于 UCore 索引的 buffer 诊断和语义高亮
- `nvim-autopairs` 的 Unreal C++ 编辑集成

### 依赖

- Neovim 0.10+
- Rust 工具链和 `cargo`
- 含 `.uproject` 的 Unreal Engine 项目
- `telescope.nvim` 或 `fzf-lua`（需要更完整的 picker 时）
- `blink.cmp`（需要 UCore 补全源时）
- `nvim-autopairs`（需要自动配对和回车展开时）
- `nvim-lspconfig` + `clangd`（需要真正的语义红线黄线和 LSP quick fix 时）

### 安装

#### 推荐组合

```lua
return {
  {
    "vlicecream/UTreeSitter.nvim",
    main = "utreesitter",
    lazy = false,
    dependencies = {
      {
        "nvim-treesitter/nvim-treesitter",
        build = ":TSUpdate",
        opts = function(_, opts)
          opts = opts or {}
          opts.auto_install = true
          opts.indent = { enable = true }
          return opts
        end,
      },
    },
    opts = {},
  },

  {
    "vlicecream/UVersionControlSystem.nvim",
    main = "uvcs",
    lazy = false,
    opts = {
      enable = true,
      prompt_on_readonly_save = true,
      provider = "auto",
      p4 = {
        command = "p4",
        -- port = "127.0.0.1:1666",
        -- user = "YourUser",
        -- client = "YourWorkspace",
      },
    },
  },

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
        "neovim/nvim-lspconfig", -- Neovim 0.11+ 可选，老环境建议保留
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
      completion = {
        enable = true,
        keymap = "<C-l>",
      },
      ui = {
        picker = "telescope",
      },
    },
  },
}
```

`UTreeSitter.nvim` 和 `UVersionControlSystem.nvim` 现在都是独立的顶层插件，`UCore.nvim` 不再内置这两层。
`extend_blink_opts()` 只在配置阶段补全 `blink.cmp` 选项，UCore 不会在运行时改写 blink 配置。

### 语义诊断

真正的 C++ 红线黄线，请交给 `clangd`。

UCore 自己主要负责：

- Unreal 专属规则诊断
- `Alt+Enter` 的智能回退修复
- 基于索引的 include 插入

未定义符号、类型不匹配、重载解析、模板错误这类语义问题，应该由 `clangd` 提供。

推荐接法：

```lua
{
  "neovim/nvim-lspconfig", -- Neovim 0.11+ 可选
}
```

`setup_clangd()` 会专门帮 Unreal 处理这些事：

- 把 `unreal_cpp` 加进 clangd filetype
- 有 `blink.cmp` 时自动补全 capabilities
- 在 Windows 上优先使用 Visual Studio LLVM 里的 `x64/bin/clangd.exe`
- 尽量自动查找附近的 `compile_commands.json`
- 把当前生效的 `compile_commands.json` 归档到 UCore 项目缓存目录，再让 clangd 指向那里
- 默认在找不到编译数据库时不 attach clangd，避免满屏错误红线
- 默认使用更保守的低内存 clangd 参数，后台索引改成按需开启

对大多数 Windows Unreal 环境来说，通常不需要手写 `clangd.exe` 路径，也不需要自己管理 `compile_commands.json` 放哪里，UCore 会优先自动处理。

也可以直接让 UCore 帮你跑 UnrealBuildTool 生成编译数据库：

```vim
:UCore debug generate-db
```

如果你的编译数据库不在项目根目录下：

```lua
require("ucore").setup({
  lsp = {
    clangd = {
      compile_commands_dir = "D:/YourProject/.vscode",
    },
  },
})
```

如果你就是要在没有 `compile_commands.json` 的情况下也强行 attach：

```lua
require("ucore").setup({
  lsp = {
    clangd = {
      require_compile_commands = false,
    },
  },
})
```

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
:UCore build [configuration] [platform] [target]
:UCore build-cancel
:UCore editor
:UCore explorer
:UCore globalfind [pattern]
:UCore goto <definition|declaration|implementation|references|source>
:UCore debug status
:UCore debug logs
:UCore debug rpc-status
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
  port = 30110,
  use_release_binary = true,
  ui = {
    picker = "telescope",
  },
  completion = {
    enable = true,
    keymap = "<C-l>",
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
  lsp = {
    clangd = {
      command = "clangd",
      compile_commands_dir = nil,
    },
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

后端优先使用 `u-scanner/target/release/` 下的 release 二进制，缺失时回退到 `cargo run`。

### 排查

```vim
:checkhealth ucore
:UCore debug status
:UCore debug logs
```

常见情况：

- 没装 Rust：从 `https://rustup.rs/` 安装
- 项目还没建索引：运行 `:UCore` 并等待 boot/index 完成
- 服务没有起来：查看 `:UCore debug logs`
- 没有语法高亮：安装 `UTreeSitter.nvim`，然后运行 `:checkhealth utreesitter`

### 相关仓库

```text
UTreeSitter                  grammar + queries + parser tests
UTreeSitter.nvim             Neovim parser/filetype/highlight integration
UVersionControlSystem.nvim   Unreal VCS dashboard and actions
UCore.nvim                   Unreal project index, RPC, navigation, completion
```

### 许可

MIT
