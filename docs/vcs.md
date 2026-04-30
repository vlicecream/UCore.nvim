# UCore VCS

UCore currently focuses on Perforce (P4). SVN support is planned, so the user-facing entry point is named `vcs` rather than `p4`.

## Requirements

- `p4` command-line client on `PATH`
- A Perforce workspace connected to the Unreal project
- A valid P4 session (`p4 login`) when your server requires authentication
- Standard P4 environment variables such as `P4PORT`, `P4USER`, and `P4CLIENT`, or explicit UCore config overrides

## Configuration

By default, UCore reads your normal P4 environment. You only need manual config when your Neovim environment cannot see the same P4 settings as your terminal.

```lua
require("ucore").setup({
  vcs = {
    provider = "p4",
    p4 = {
      command = "p4",
      port = nil,
      user = nil,
      client = nil,
      charset = nil,
      config = nil,
      env = nil,
    },
  },
})
```

## Dashboard

`:UCore vcs` opens the main source-control dashboard. `:UCore changes` also opens this dashboard.

The dashboard is intentionally P4V/LazyGit-like:

| Area | Purpose |
|------|---------|
| Header | Provider, project, workspace, and user |
| Left pane | Workspace files, writable files, changelists, and shelves |
| Right pane | File summary, diff, changelist detail, or shelf detail |
| Footer | Context-aware key hints |

Common keys:

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection |
| `Space` | Toggle file checked |
| `Enter` | Open file, expand changelist, or expand shelf |
| `d` | Load diff for the selected item |
| `c` | Checkout/edit selected file with `p4 edit` |
| `a` | Add a new local file with `p4 add` |
| `r` | Revert an opened P4 file |
| `m` | Open commit UI with checked files |
| `R` | Refresh dashboard data |
| `q` / `Esc` | Close dashboard |

The footer is context-aware. For writable files that are not opened in P4, `r revert` is hidden because P4 cannot safely revert them yet. Use `c checkout` first, then `r revert`.

## Read-Only Editing

When you press an edit key such as `i`, `a`, `o`, or `O` on a read-only P4 file, UCore checks the file before entering Insert mode.

If the file is not opened in P4, UCore asks:

```text
P4 checkout/edit
Make writable only
Cancel
```

- `P4 checkout/edit` runs `p4 edit`, makes the file writable, then continues the original edit key.
- `Make writable only` only changes the local file attribute. It does not open the file in P4.
- `Cancel` keeps the file unchanged and does not enter Insert mode.

If the file is already opened in P4 but still has a read-only local attribute, UCore silently makes it writable and continues editing.

`BufWritePre` keeps the same safety net for edits created by paste, commands, or other plugins.

## Writable Files

Writable files are files that look locally modified or writable but are not opened in P4.

Dashboard behavior:

- They are shown under `Writable Files`.
- They cannot be committed directly.
- Press `c` to run `p4 edit`.
- After checkout, they move into the normal workspace/changelist flow.
- `r revert` is hidden for these rows until they are opened in P4.

## Revert

For opened P4 files, `r` runs `p4 revert` after confirmation.

If the same file is open in a Neovim buffer, UCore reloads that buffer from disk after the revert succeeds. If the buffer has unsaved changes, the confirmation message warns that those unsaved changes will also be discarded.

## Commit

`:UCore commit` opens the visual commit UI. The dashboard `m` key opens the same UI using the checked files.

Before commit, UCore checks for unsaved project buffers:

- If no project buffers are dirty, commit continues.
- If project buffers are dirty, UCore asks whether to save all and continue.
- If you cancel, commit is aborted and your commit message is not submitted.

The commit UI groups files by changelist. It submits opened P4 files only; writable files must be checked out first.

## Changelists and Shelves

Pending changelists and shelves are displayed in the dashboard as expandable groups.

- Press `Enter` on a changelist or shelf to expand/collapse it.
- Press `d` on a shelf to load shelf diff/detail.
- Changelist descriptions are shown instead of forcing you to read raw changelist IDs first.

## Useful Commands

```vim
:UCore vcs               " Open VCS dashboard
:UCore changes           " Open VCS dashboard focused on changes
:UCore checkout          " p4 edit current file
:UCore commit            " Open visual commit UI
:UCore changelists       " View pending changelists
:UCore debug vcs         " Print VCS diagnostics
```

## Troubleshooting

| Symptom | Check |
|---------|-------|
| No P4 provider detected | Run `p4 info -s` in the same shell/environment |
| Login required | Run `:UCore vcs login` or `p4 login` |
| Read-only prompt does not appear | Confirm VCS is enabled and the file belongs to an Unreal project |
| Writable file cannot commit | Press `c` in the dashboard to open it with `p4 edit` |
| Dashboard shows stale data | Press `R` |
| Revert did not change current buffer | Make sure the revert succeeded; opened buffers are reloaded after success |
