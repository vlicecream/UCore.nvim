local project = require("ucore.project")
local vcs = require("ucore.vcs")

local M = {}

local state = nil

local SEP = string.rep("━", 54)

local function collect_data(root, provider)
  local data = {
    root = root,
    provider = provider,
    info = {},
    opened = {},
    local_files = {},
    changelists = {},
    opened_start = 0,
    opened_end = 0,
    local_start = 0,
    local_end = 0,
    changes_start = 0,
    changes_end = 0,
  }

  local info, _ = provider.info(root)
  data.info = info or {}

  local opened = provider.opened(root) or {}
  for _, f in ipairs(opened) do
    table.insert(data.opened, { path = f.path, status = f.action, checked = true })
  end

  local local_changes = provider.status(root) or {}
  for _, f in ipairs(local_changes) do
    local already = false
    for _, o in ipairs(data.opened) do
      if o.path:lower() == f.path:lower() then already = true; break end
    end
    if not already then
      table.insert(data.local_files, { path = f.path, status = f.status, checked = false })
    end
  end

  if provider.pending_changelists then
    data.changelists = provider.pending_changelists(root) or {}
  end

  return data
end

local function build_lines(data)
  local lines = {
    "UCore VCS Dashboard",
    SEP,
    "Provider: P4",
    "Project:  " .. vim.fn.fnamemodify(data.root, ":t"),
    "Client:   " .. tostring(data.info["client name"] or "?"),
    "User:     " .. tostring(data.info["user name"] or "?"),
    "Root:     " .. tostring(data.info["client root"] or data.root),
    "",
    "Opened Files",
  }

  data.opened_start = #lines + 1
  if #data.opened == 0 then
    table.insert(lines, "  (none)")
  else
    for _, f in ipairs(data.opened) do
      local mark = f.checked and "[x]" or "[ ]"
      table.insert(lines, string.format("  %s  %-6s %s", mark, f.status, f.path))
    end
  end
  data.opened_end = #lines

  table.insert(lines, "")
  table.insert(lines, "Local Candidates")
  data.local_start = #lines + 1
  if #data.local_files == 0 then
    table.insert(lines, "  (none)")
  else
    for _, f in ipairs(data.local_files) do
      local mark = f.checked and "[x]" or "[ ]"
      table.insert(lines, string.format("  %s  %-6s %s", mark, f.status, f.path))
    end
  end
  data.local_end = #lines

  table.insert(lines, "")
  table.insert(lines, "Pending Changelists")
  data.changes_start = #lines + 1
  if #data.changelists == 0 then
    table.insert(lines, "  (none)")
  else
    for _, ch in ipairs(data.changelists) do
      local desc = ch.description:gsub("\n", " "):sub(1, 60)
      table.insert(lines, string.format("  %d  %s", ch.number, desc))
    end
  end
  data.changes_end = #lines

  table.insert(lines, "")
  table.insert(lines, SEP)
  table.insert(lines, "<Tab> toggle   o open   d diff   c checkout   a add")
  table.insert(lines, "r revert       m commit l changelist         R refresh q close")

  return lines
end

function M.list(root)
  if not root then
    root = project.find_project_root()
    if not root then
      vim.notify("UCore: no Unreal project detected", vim.log.levels.ERROR)
      return
    end
  end

  local provider = vcs.detect(root)
  if not provider then
    vim.notify("UCore: no P4 provider detected", vim.log.levels.WARN)
    return
  end

  local data = collect_data(root, provider)
  local lines = build_lines(data)

  vim.cmd("botright 22new")
  local buf = vim.api.nvim_get_current_buf()
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "ucore-vcs"
  vim.bo[buf].modified = false
  pcall(vim.api.nvim_buf_set_name, buf, "ucore://vcs-dashboard/" .. tostring(buf))
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modified = false
  vim.api.nvim_set_option_value("cursorline", true, { buf = buf })

  state = { buf = buf, root = root, provider = provider, data = data }
  setup_keymaps(buf)

  vim.api.nvim_set_current_buf(buf)
end

local function find_item_at_line(buf, line, data)
  if line >= data.opened_start and line <= data.opened_end then
    local idx = line - data.opened_start + 1
    local f = data.opened[idx]
    if f then return f, "opened", idx end
  end
  if line >= data.local_start and line <= data.local_end then
    local idx = line - data.local_start + 1
    local f = data.local_files[idx]
    if f then return f, "local", idx end
  end
  if line >= data.changes_start and line <= data.changes_end then
    local idx = line - data.changes_start + 1
    local ch = data.changelists[idx]
    if ch then return ch, "changelist", idx end
  end
  return nil, nil, nil
end

local function is_item_line(buf, line)
  if not state then return false end
  local d = state.data
  return (line >= d.opened_start and line <= d.opened_end)
      or (line >= d.local_start and line <= d.local_end)
      or (line >= d.changes_start and line <= d.changes_end)
end

local function toggle_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind, idx = find_item_at_line(buf, cur, state.data)
  if not item or kind == "changelist" then
    vim.notify("UCore: move cursor to a file to toggle", vim.log.levels.INFO)
    return
  end
  item.checked = not item.checked
  local mark = item.checked and "[x]" or "[ ]"
  local lc = vim.api.nvim_buf_get_lines(buf, cur - 1, cur, false)[1] or ""
  local nc = lc:gsub("%[.%]", mark)
  vim.api.nvim_buf_set_lines(buf, cur - 1, cur, false, { nc })
end

local function open_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if not item then return end
  if kind == "changelist" then
    require("ucore.vcs.changelists").detail(state.root, state.provider, item.number)
    return
  end
  if item.path and vim.fn.filereadable(item.path) == 1 then
    vim.cmd.edit(vim.fn.fnameescape(item.path))
  end
end

local function diff_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if not item or kind == "changelist" or not item.path then return end
  local text, err = state.provider.diff(item.path)
  if err then return vim.notify("UCore: " .. tostring(err), vim.log.levels.ERROR) end
  if not text or text == "" then return vim.notify("UCore: no diff", vim.log.levels.INFO) end
  vim.cmd("botright 12new")
  local dbuf = vim.api.nvim_get_current_buf()
  vim.bo[dbuf].buftype = "nofile"
  vim.bo[dbuf].bufhidden = "wipe"
  vim.bo[dbuf].swapfile = false
  vim.bo[dbuf].filetype = "diff"
  local dl = vim.split(text, "\n", { plain = true })
  vim.api.nvim_buf_set_lines(dbuf, 0, -1, false, dl)
  vim.bo[dbuf].modified = false
end

local function checkout_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if not item or not item.path then return end
  local ok, err = state.provider.checkout(item.path)
  if ok then
    item.checked = true
    local lc = vim.api.nvim_buf_get_lines(buf, cur - 1, cur, false)[1] or ""
    local nc = lc:gsub("%[.%]", "[x]")
    vim.api.nvim_buf_set_lines(buf, cur - 1, cur, false, { nc })
    vim.notify("UCore: p4 edit " .. vim.fn.fnamemodify(item.path, ":t"), vim.log.levels.INFO)
  else
    vim.notify("UCore: p4 edit failed: " .. tostring(err), vim.log.levels.ERROR)
  end
end

local function add_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if not item or kind ~= "local" then
    vim.notify("UCore: move cursor to a local candidate to add", vim.log.levels.INFO)
    return
  end
  if not state.provider.add_file then
    vim.notify("UCore: p4 add not available", vim.log.levels.WARN)
    return
  end
  local ok, err = state.provider.add_file(item.path)
  if ok then
    vim.notify("UCore: p4 add " .. vim.fn.fnamemodify(item.path, ":t"), vim.log.levels.INFO)
    vim.api.nvim_buf_delete(buf, { force = true })
    state = nil
    M.list(state and state.root or nil)
  else
    vim.notify("UCore: p4 add failed: " .. tostring(err), vim.log.levels.ERROR)
  end
end

local function revert_item(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if not item or not item.path then return end
  local confirm = vim.fn.confirm("UCore: revert " .. vim.fn.fnamemodify(item.path, ":t") .. "?", "&Revert\n&Cancel", 2, "Question")
  if confirm ~= 1 then return end
  state.provider.do_revert(item.path)
  vim.notify("UCore: reverted " .. vim.fn.fnamemodify(item.path, ":t"), vim.log.levels.INFO)
  vim.api.nvim_buf_delete(buf, { force = true })
  state = nil
  M.list(state and state.root or nil)
end

local function commit_selected(buf)
  local paths = {}
  for _, f in ipairs(state.data.opened) do
    if f.checked then table.insert(paths, f.path) end
  end
  for _, f in ipairs(state.data.local_files) do
    if f.checked then table.insert(paths, f.path) end
  end
  vim.api.nvim_buf_delete(buf, { force = true })
  state = nil
  require("ucore.vcs.commit").open(state and state.root or nil)
end

local function open_changelist(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local item, kind = find_item_at_line(buf, cur, state.data)
  if item and kind == "changelist" then
    require("ucore.vcs.changelists").detail(state.root, state.provider, item.number)
  end
end

local function refresh(buf)
  vim.api.nvim_buf_delete(buf, { force = true })
  state = nil
  M.list(state and state.root or nil)
end

local function close(buf)
  vim.api.nvim_buf_delete(buf, { force = true })
  state = nil
end

local function setup_keymaps(buf)
  local opts = { buffer = buf, nowait = true, silent = true }

  vim.keymap.set("n", "<Tab>", function() toggle_item(buf) end, opts)
  vim.keymap.set("n", "o", function() open_item(buf) end, opts)
  vim.keymap.set("n", "d", function() diff_item(buf) end, opts)
  vim.keymap.set("n", "c", function() checkout_item(buf) end, opts)
  vim.keymap.set("n", "a", function() add_item(buf) end, opts)
  vim.keymap.set("n", "r", function() revert_item(buf) end, opts)
  vim.keymap.set("n", "m", function() commit_selected(buf) end, opts)
  vim.keymap.set("n", "l", function() open_changelist(buf) end, opts)
  vim.keymap.set("n", "R", function() refresh(buf) end, opts)
  vim.keymap.set("n", "q", function() close(buf) end, opts)
end

function M.open()
  M.list()
end

return M
