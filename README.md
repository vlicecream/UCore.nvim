# UCore.nvim

UCore.nvim is a Neovim plugin for Unreal Engine projects.

It uses a Rust backend, `u-scanner`, to index Unreal C++ source files, modules,
assets, config files, symbols, definitions, references, and project metadata.
The Lua frontend exposes that index inside Neovim through commands, Telescope
pickers, completion sources, semantic highlights, and health checks.

> Status: early and evolving. The core workflow is usable, but APIs may still
> change while the plugin matures.

## Features

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

## Requirements

- Neovim 0.10+
- Rust toolchain with `cargo`
- An Unreal Engine project with a `.uproject` file
- `nvim-treesitter` if you want Unreal C++ Tree-sitter highlighting
- `telescope.nvim` or `fzf-lua` if you want a richer picker UI
- `blink.cmp` if you want UCore as a completion source

Windows is the primary development target today.

## Installation

### lazy.nvim

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

### Local development

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

## First Run

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
4. Create or reuse project databases under Neovim's cache directory.
5. Refresh the project index if needed.
6. Start file watching.
7. Refresh the shared Unreal Engine index if needed.

With `auto_boot = true`, booting also happens automatically when opening files
in an Unreal project.

## Commands

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
status, and registered project count. Items are laid out in fixed-width columns
for easy scanning. Actions that need a project or an index show a helpful
redirect when the prerequisite is missing.

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

## Configuration

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

## Rust Backend

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

## Unreal Build and Editor

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

## Tree-sitter

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

## Completion

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

## Runtime Data

UCore stores runtime data under Neovim's cache directory. It does not write
databases into your Unreal project by default.

Typical paths:

```text
stdpath("cache")/ucore/registry.json
stdpath("cache")/ucore/server-registry.json
stdpath("cache")/ucore/projects/<project-name>-<hash>/ucore.db
stdpath("cache")/ucore/projects/<project-name>-<hash>/ucore-cache.db
stdpath("cache")/ucore/projects/<project-name>-<hash>/u_core_server.log
stdpath("cache")/ucore/engines/<engine-id>/engine.db
stdpath("cache")/ucore/engines/<engine-id>/engine-cache.db
```

Use this to inspect the current paths:

```vim
:UCore status
```

## Troubleshooting

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

## Development Notes

The Lua frontend talks to the Rust backend in two ways:

1. CLI bridge through `u_scanner`.
2. TCP + MessagePack RPC through `u_core_server`.

Interactive features prefer RPC. CLI is still used for lifecycle and fallback
paths.

## License

MIT
