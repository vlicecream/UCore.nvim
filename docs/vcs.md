# UCore VCS

P4 (Perforce) is the supported version control system. SVN support is planned.

## Requirements

- `p4` command-line client on `PATH`
- A Perforce workspace connected to the Unreal project
- Environment variables: `P4PORT`, `P4USER`, `P4CLIENT` (or use config overrides)

## Manual P4 Configuration

```lua
require("ucore").setup({
  vcs = {
    provider = "p4",
    p4 = {
      command = "p4",         -- path to p4 executable
      port = nil,             -- override P4PORT
      user = nil,             -- override P4USER
      client = nil,           -- override P4CLIENT
      charset = nil,          -- override P4CHARSET
      config = nil,           -- override P4CONFIG
      env = {},               -- extra environment variables
    },
  },
})
```

## Dashboard

`:UCore vcs` opens a LazyGit-style floating window:

| View | Description |
|------|-------------|
| Header | Provider, project name, client, user |
| Left pane | Workspace info, file changes, pending changelists |
| Right pane | Diff preview, file content, changelist detail |
| Footer | Keybinding reference |

### Controls

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection up/down |
| `Space` | Toggle file checked |
| `Enter` | Open file / show changelist detail |
| `d` | Refresh right pane as diff |
| `c` | `p4 edit` selected file |
| `a` | `p4 add` selected local candidate |
| `r` | Revert file (with confirmation) |
| `m` | Open commit UI with checked files |
| `l` | Show changelist detail |
| `s` | Submit selected changelist |
| `R` | Refresh all data |
| `?` | Show keybinding help |
| `q` / `Esc` | Close dashboard |

## Read-Only Save Prompt

When saving a buffer backed by a P4-opened file that is writable, nothing happens. When saving a read-only file that is not checked out, UCore shows a prompt:

```
File is read-only in P4.
Checkout (p4 edit) and retry?
```

Selecting yes runs `p4 edit` then retries the save.

## Commit UI

`:UCore commit` opens a scratch buffer:

```text
UCore Commit
════════════════════════════════════════════════════════════════════════
VCS: P4
Project: SimpleBeta
Root: C:/Users/.../SimpleBeta
Client: my_client
User: peng.xu

Files:
  [x]  opened  edit    SHero.cpp
  [x]  opened  edit    SHero.h
  [ ]  local   add?    NewAbility.cpp

Message:
<type commit message here>

════════════════════════════════════════════════════════════════════════
<Tab> toggle   <C-s> submit   d diff   a add   r revert   q close
```

Controls:
- `<Tab>` — toggle file checked
- `<C-s>` — submit (after confirmation)
- `d` — show diff for file under cursor
- `a` — `p4 add` local candidate under cursor
- `r` — revert file (with confirmation)
- `q` — close

## Changelist Recovery

If a submit fails, the changelist is preserved:

```
UCore submit failed.
Changelist 12345 was kept.
Run :UCore changelists
```

Use `:UCore changelists` to view pending changelists, inspect their files, and submit individually.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| No P4 provider detected | Run `p4 info -s` in terminal |
| Dashboard shows no changes | Run `p4 opened` and `p4 status` manually |
| Checkout fails | Verify P4 connection and workspace mapping |
| Commit fails | Check P4 permissions and submit form |
| Server errors in logs | Check `:UCore debug logs` |
