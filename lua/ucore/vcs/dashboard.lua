local project = require("ucore.project")
local p4 = require("ucore.vcs.p4")

local M = {}

local state = nil
local ns = vim.api.nvim_create_namespace("ucore_vcs_dashboard")
local autocmd_group = nil

local HIGHLIGHTS = {
  UCoreVcsBorder     = { link = "NormalFloat" },
  UCoreVcsTitle      = { link = "Title" },
  UCoreVcsHeader     = { link = "NormalFloat" },
  UCoreVcsSection    = { link = "Statement" },
  UCoreVcsSelected   = { link = "CursorLine" },
  UCoreVcsChecked    = { link = "Operator" },
  UCoreVcsUnchecked  = { link = "NonText" },
  UCoreVcsStatusEdit = { fg = "#4ec9b0" },
  UCoreVcsStatusAdd  = { fg = "#6a9955" },
  UCoreVcsStatusDel  = { fg = "#f14c4c" },
  UCoreVcsStatusLocal= { fg = "#dcdcaa" },
  UCoreVcsFilename   = { link = "Function" },
  UCoreVcsDir        = { link = "Comment" },
  UCoreVcsChangelistNum = { link = "Number" },
  UCoreVcsChangelistDesc = { link = "String" },
  UCoreVcsHelp       = { link = "NonText" },
  UCoreVcsMuted      = { link = "Comment" },
}

for name, opts in pairs(HIGHLIGHTS) do
  pcall(vim.api.nvim_set_hl, 0, name, opts)
end

local function split_path(path)
  local name = vim.fn.fnamemodify(path, ":t")
  local dir = vim.fn.fnamemodify(path, ":h")
  return name, dir
end

local function normalize_path(path)
  return tostring(path or ""):gsub("\\", "/")
end

local function compact_directory(path, width)
  path = normalize_path(path)
  width = width or 28
  if path == "" then
    return ""
  end

  local root = state and state.root and normalize_path(state.root) or ""
  if root ~= "" and path:lower():sub(1, #root) == root:lower() then
    path = path:sub(#root + 2)
  end

  if vim.fn.strdisplaywidth(path) <= width then
    return path
  end

  local tail = path:sub(math.max(1, #path - width + 4))
  return "..." .. tail
end

local function is_add_candidate(item)
  return item and item.kind == "file" and item.section == "local" and item.status == "add?"
end

local function is_modify_candidate(item)
  return item and item.kind == "file" and item.section == "local" and item.status == "modify?"
end

local function file_status_label(raw_status)
  if raw_status == "open for add" or raw_status == "add" or raw_status == "a" or raw_status == "?" then
    return "add?"
  end
  return "modify?"
end

function M.collect_data(root, filter)
  filter = filter or "all"
  local info, _ = p4.info(root)
  info = info or {}

  local opened = p4.opened(root) or {}
  local local_changes = p4.status(root) or {}
  local changes = {}
  if p4.pending_changelists then
    changes = p4.pending_changelists(root) or {}
  end

  local local_seen = {}
  for _, f in ipairs(opened) do
    local_seen[f.path:lower()] = true
  end

  local proj_name = vim.fn.fnamemodify(root, ":t")
  local rows = {}

  table.insert(rows, { kind = "section", label = "Workspace" })
  table.insert(rows, { kind = "info", label = "Root",    value = root })
  table.insert(rows, { kind = "info", label = "Client",  value = info["client name"] or "?" })
  table.insert(rows, { kind = "info", label = "User",    value = info["user name"] or "?" })

  if filter ~= "changelists" then
    table.insert(rows, { kind = "blank" })
    table.insert(rows, { kind = "section", label = "Changes" })

    local has_changes = false
    for _, f in ipairs(opened) do
      has_changes = true
      local name, dir = split_path(f.path)
      table.insert(rows, {
        kind = "file", section = "opened", checked = true,
        status = f.action, raw_status = f.action,
        path = f.path, filename = name, directory = dir,
      })
    end
    for _, f in ipairs(local_changes) do
      if not local_seen[f.path:lower()] then
        has_changes = true
        local name, dir = split_path(f.path)
        table.insert(rows, {
          kind = "file", section = "local", checked = false,
          status = file_status_label(f.status), raw_status = f.status,
          path = f.path, filename = name, directory = dir,
        })
      end
    end
    if not has_changes then
      table.insert(rows, { kind = "empty", text = "  (no changes)" })
    end
  end

  if filter ~= "files" then
    table.insert(rows, { kind = "blank" })
    table.insert(rows, { kind = "section", label = "Pending Changelists" })

    if #changes > 0 then
      for _, ch in ipairs(changes) do
        table.insert(rows, {
          kind = "changelist",
          number = ch.number,
          description = ch.description:gsub("\n", " "):sub(1, 60),
          user = ch.user,
        })
      end
    else
      table.insert(rows, { kind = "empty", text = "  (none)" })
    end
  end

  return {
    root = root,
    project_name = proj_name,
    info = info,
    rows = rows,
    cursor = 1,
  }
end

local function cursor_to_first_selectable()
  if not state then return end
  for i, row in ipairs(state.rows) do
    if row.kind == "file" or row.kind == "changelist" then
      state.cursor = i
      return
    end
  end
  state.cursor = 1
end

local function get_current_item()
  if not state then return nil end
  return state.rows[state.cursor]
end

local function is_selectable(row)
  return row and (row.kind == "file" or row.kind == "changelist")
end

local function move_cursor(delta)
  if not state then return end
  local n = #state.rows
  local pos = state.cursor
  for _ = 1, n do
    pos = ((pos - 1 + delta) % n + n) % n + 1
    if is_selectable(state.rows[pos]) then
      break
    end
  end
  if is_selectable(state.rows[pos]) then
    state.cursor = pos
    M.render_left()
    M.render_right()
  end
end

local function will_fit()
  local min_left = 50
  local min_right = 30
  local min_height = 15
  return vim.o.columns >= min_left + min_right + 2
     and vim.o.lines >= min_height + 4
end

local function open_windows()
  if not will_fit() then
    vim.notify("UCore VCS: terminal too small for dashboard", vim.log.levels.WARN)
    return false
  end

  local ed_w = vim.o.columns
  local ed_h = vim.o.lines

  local left_w = math.max(46, math.floor(ed_w * 0.52))
  local right_w = ed_w - left_w - 4
  if right_w < 30 then
    left_w = ed_w - 34
    right_w = 30
  end

  local h = math.min(ed_h - 2, 38)
  local row = math.max(0, math.floor((ed_h - h) / 2))
  local col = math.max(0, math.floor((ed_w - left_w - right_w - 4) / 2))

  local success, result = pcall(function()
    local header_buf = vim.api.nvim_create_buf(false, true)
    local header_win = vim.api.nvim_open_win(header_buf, true, {
      relative = "editor",
      width = left_w + right_w + 4,
      height = 3,
      row = row,
      col = col,
      style = "minimal",
      border = "single",
      title = " UCore VCS ",
      title_pos = "center",
    })
    vim.bo[header_buf].modifiable = true

    local left_buf = vim.api.nvim_create_buf(false, true)
    local left_win = vim.api.nvim_open_win(left_buf, false, {
      relative = "editor",
      width = left_w + 2,
      height = h - 3,
      row = row + 3,
      col = col,
      style = "minimal",
      border = "single",
    })
    vim.bo[left_buf].modifiable = true
    vim.wo[left_win].cursorline = false
    vim.wo[left_win].cursorlineopt = "line"

    local right_buf = vim.api.nvim_create_buf(false, true)
    local right_win = vim.api.nvim_open_win(right_buf, false, {
      relative = "editor",
      width = right_w + 2,
      height = h - 3,
      row = row + 3,
      col = col + left_w + 2 + 2,
      style = "minimal",
      border = "single",
    })
    vim.bo[right_buf].modifiable = true

    local footer_buf = vim.api.nvim_create_buf(false, true)
    local footer_win = vim.api.nvim_open_win(footer_buf, false, {
      relative = "editor",
      width = left_w + right_w + 4,
      height = 1,
      row = row + h + 1,
      col = col,
      style = "minimal",
      border = "none",
    })
    vim.bo[footer_buf].modifiable = true

    vim.api.nvim_set_option_value("winhl", "Normal:NormalFloat", { win = header_win })
    vim.api.nvim_set_option_value("winhl", "Normal:NormalFloat", { win = left_win })
    vim.api.nvim_set_option_value("winhl", "Normal:NormalFloat", { win = right_win })
    vim.api.nvim_set_option_value("winhl", "Normal:NormalFloat", { win = footer_win })

    return {
      header_buf = header_buf, header_win = header_win,
      left_buf = left_buf, left_win = left_win,
      right_buf = right_buf, right_win = right_win,
      footer_buf = footer_buf, footer_win = footer_win,
    }
  end)

  if not success then
    vim.notify("UCore VCS: failed to create windows: " .. tostring(result), vim.log.levels.ERROR)
    return nil
  end

  return result
end

function M.close()
  if not state then return end
  if autocmd_group then
    pcall(vim.api.nvim_del_augroup_by_id, autocmd_group)
    autocmd_group = nil
  end
  if state.wins then
    local w = state.wins
    pcall(vim.api.nvim_win_close, w.header_win, true)
    pcall(vim.api.nvim_win_close, w.left_win, true)
    pcall(vim.api.nvim_win_close, w.right_win, true)
    pcall(vim.api.nvim_win_close, w.footer_win, true)
    pcall(vim.api.nvim_buf_delete, w.header_buf, { force = true })
    pcall(vim.api.nvim_buf_delete, w.left_buf, { force = true })
    pcall(vim.api.nvim_buf_delete, w.right_buf, { force = true })
    pcall(vim.api.nvim_buf_delete, w.footer_buf, { force = true })
  end
  state = nil
end

function M.render_header()
  if not state or not state.wins then return end
  local buf = state.wins.header_buf
  vim.bo[buf].modifiable = true
  local info = state.info
  local client = info["client name"] or "?"
  local user = info["user name"] or "?"
  local left = string.format("P4 | %s", state.project_name)
  local client_part = string.format("Client: %s", client)
  local width = vim.api.nvim_win_get_width(state.wins.header_win)
  local pad = math.max(2, width - vim.fn.strdisplaywidth(left) - vim.fn.strdisplaywidth(client_part) - 3)
  local line1 = " " .. left .. string.rep(" ", pad) .. client_part
  local line2 = " " .. string.rep(" ", #left + pad) .. string.format("User: %s", user)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { line1, line2 })
  vim.bo[buf].modifiable = false
end

function M.render_footer()
  if not state or not state.wins then return end
  local buf = state.wins.footer_buf
  vim.bo[buf].modifiable = true
  local lines = {
    " j/k move  Space toggle  Enter open  d diff  c checkout  a add  r revert  m commit  l changelist  s submit  R refresh  ? help  q close",
  }
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
end

function M.render_left()
  if not state or not state.wins then return end
  local buf = state.wins.left_buf
  local rows = state.rows

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, {})

  local text_lines = {}
  for _, row in ipairs(rows) do
    if row.kind == "section" then
      table.insert(text_lines, row.label)
    elseif row.kind == "info" then
      table.insert(text_lines, string.format("  %-7s %s", row.label, row.value))
    elseif row.kind == "blank" then
      table.insert(text_lines, "")
    elseif row.kind == "empty" then
      table.insert(text_lines, row.text or "")
    elseif row.kind == "file" then
      local mark = row.checked and "[x]" or "[ ]"
      local dir = compact_directory(row.directory, 30)
      if dir ~= "" then
        table.insert(text_lines, string.format("  %s  %-7s %-28s %s", mark, row.status, row.filename, dir))
      else
        table.insert(text_lines, string.format("  %s  %-7s %s", mark, row.status, row.filename))
      end
    elseif row.kind == "changelist" then
      local desc = (row.description or ""):gsub("\n", " "):sub(1, 55)
      table.insert(text_lines, string.format("  %-6d  %s", row.number, desc))
    end
  end

  if #text_lines == 0 then
    table.insert(text_lines, "(no data)")
  end

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, text_lines)
  vim.bo[buf].modifiable = false
  vim.bo[buf].modified = false

  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)

  for i, row in ipairs(rows) do
    local line = i - 1
    local line_text = text_lines[line + 1] or ""

    if row.kind == "section" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSection", line, 0, #line_text)
    elseif row.kind == "info" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsMuted", line, 0, #line_text)
    elseif row.kind == "file" then
      local mark_begin = line_text:find("%[.%]")
      if mark_begin then
        vim.api.nvim_buf_add_highlight(buf, ns,
          row.checked and "UCoreVcsChecked" or "UCoreVcsUnchecked",
          line, mark_begin - 1, mark_begin + 2)
      end
      local stat_end = (mark_begin or 0) + 4 + 7
      local sg = "UCoreVcsStatusEdit"
      if row.status == "add" or row.status == "add?" then sg = "UCoreVcsStatusAdd"
      elseif row.status == "delete" or row.status == "delete?" then sg = "UCoreVcsStatusDel"
      elseif row.section == "local" then sg = "UCoreVcsStatusLocal" end
      vim.api.nvim_buf_add_highlight(buf, ns, sg, line, (mark_begin or 0) + 3, math.min(stat_end, #line_text))
      local fn_start = line_text:find(row.filename, 1, true)
      if fn_start then
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsFilename", line, fn_start - 1, fn_start - 1 + #row.filename)
      end
      local compact_dir = compact_directory(row.directory, 30)
      local dir_start = compact_dir ~= "" and line_text:find(compact_dir, 1, true) or nil
      if dir_start then
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsDir", line, dir_start - 1, #line_text)
      end
    elseif row.kind == "changelist" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsChangelistNum", line, 2, 8)
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsChangelistDesc", line, 9, #line_text)
    end
  end

  if is_selectable(state.rows[state.cursor]) then
    local sel_line = state.cursor - 1
    vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSelected", sel_line, 0, -1)
    pcall(vim.api.nvim_win_set_cursor, state.wins.left_win, { sel_line + 1, 0 })
  end
end

function M.render_right()
  if not state or not state.wins then return end
  local buf = state.wins.right_buf
  local item = get_current_item()

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, {})

  if not item then
    vim.bo[buf].modifiable = false
    return
  end

  if item.kind == "file" then
    local diff_text, diff_err = p4.diff(item.path)
    local title = {
      "File: " .. normalize_path(item.path),
      "Status: " .. tostring(item.status or ""),
      "",
    }
    if diff_text and diff_text ~= "" then
      local lines = vim.split(diff_text, "\n", { plain = true })
      vim.list_extend(title, lines)
      vim.api.nvim_buf_set_lines(buf, 0, -1, false, title)
      vim.bo[buf].filetype = "diff"
    elseif item.path and vim.fn.filereadable(item.path) == 1 then
      local ok, lines = pcall(vim.fn.readfile, item.path, "", 500)
      if ok then
        vim.list_extend(title, lines)
        vim.api.nvim_buf_set_lines(buf, 0, -1, false, title)
        local ft = vim.filetype.match({ filename = item.path }) or ""
        vim.bo[buf].filetype = ft
      end
    end
  elseif item.kind == "changelist" then
    local detail, detail_err = p4.changelist_detail(item.number)
    if detail then
      local lines = {
        "Change " .. tostring(detail.number),
        "User: " .. detail.user,
        "",
        "Description:",
        "  " .. detail.description,
        "",
        "Files:",
      }
      for _, f in ipairs(detail.files or {}) do
        table.insert(lines, "  " .. f.status .. "  " .. f.path)
      end
      vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
      vim.bo[buf].filetype = "ucore-vcs-detail"
    end
  end

  vim.bo[buf].modifiable = false
  vim.bo[buf].modified = false
end

local function setup_keymaps()
  if not state or not state.wins then return end
  local buf = state.wins.left_buf
  local opts = { buffer = buf, nowait = true, silent = true }

  vim.keymap.set("n", "j", function() move_cursor(1) end, opts)
  vim.keymap.set("n", "<Down>", function() move_cursor(1) end, opts)
  vim.keymap.set("n", "k", function() move_cursor(-1) end, opts)
  vim.keymap.set("n", "<Up>", function() move_cursor(-1) end, opts)

  vim.keymap.set("n", " ", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then return end
    item.checked = not item.checked
    M.render_left()
  end, opts)

  vim.keymap.set("n", "<CR>", function()
    local item = get_current_item()
    if not item then return end
    if item.kind == "file" and item.path and vim.fn.filereadable(item.path) == 1 then
      M.close()
      vim.cmd.edit(vim.fn.fnameescape(item.path))
    elseif item.kind == "changelist" then
      local detail, err = p4.changelist_detail(item.number)
      if detail then
        M.render_right()
      end
    end
  end, opts)

  vim.keymap.set("n", "d", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then
      vim.notify("UCore: move to a file row", vim.log.levels.INFO)
      return
    end
    M.render_right()
  end, opts)

  vim.keymap.set("n", "c", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then
      vim.notify("UCore: move to a file row", vim.log.levels.INFO)
      return
    end
    if item.section == "opened" then
      vim.notify("UCore: " .. item.filename .. " is already opened", vim.log.levels.INFO)
      return
    end
    if is_add_candidate(item) then
      vim.notify("UCore: this looks like a new file. Use 'a' to p4 add it.", vim.log.levels.INFO)
      return
    end
    local ok, err = p4.checkout(item.path)
    if ok then
      item.checked = true
      item.section = "opened"
      item.status = "edit"
      vim.notify("UCore: p4 edit " .. item.filename, vim.log.levels.INFO)
      M.render_left()
    else
      vim.notify("UCore: p4 edit failed: " .. tostring(err), vim.log.levels.ERROR)
    end
  end, opts)

  vim.keymap.set("n", "a", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then
      vim.notify("UCore: move to a file row", vim.log.levels.INFO)
      return
    end
    if not is_add_candidate(item) then
      if is_modify_candidate(item) then
        vim.notify("UCore: modified local files should use 'c' for p4 edit, not add.", vim.log.levels.INFO)
      else
        vim.notify("UCore: only new local files can be added.", vim.log.levels.INFO)
      end
      return
    end
    local ok, err = p4.add_file(item.path)
    if ok then
      item.checked = true
      item.section = "opened"
      item.status = "add"
      vim.notify("UCore: p4 add " .. item.filename, vim.log.levels.INFO)
      M.render_left()
    else
      vim.notify("UCore: p4 add failed: " .. tostring(err), vim.log.levels.ERROR)
    end
  end, opts)

  vim.keymap.set("n", "r", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then
      vim.notify("UCore: move to a file row", vim.log.levels.INFO)
      return
    end
    local confirm = vim.fn.confirm(
      "UCore: revert " .. item.filename .. "?\nThis discards local changes.",
      "&Revert\n&Cancel", 2, "Question"
    )
    if confirm ~= 1 then return end
    p4.do_revert(item.path)
    vim.notify("UCore: reverted " .. item.filename, vim.log.levels.INFO)
    M.refresh()
  end, opts)

  vim.keymap.set("n", "m", function()
    local checked = {}
    for _, row in ipairs(state.rows) do
      if row.kind == "file" and row.checked then
        table.insert(checked, row.path)
      end
    end
    if #checked == 0 then
      vim.notify("UCore: no files selected for commit", vim.log.levels.WARN)
      return
    end
    local root = state.root
    M.close()
    vim.schedule(function()
      require("ucore.vcs.commit").open(root, { files = checked })
    end)
  end, opts)

  vim.keymap.set("n", "l", function()
    local item = get_current_item()
    if not item or item.kind ~= "changelist" then
      vim.notify("UCore: move to a changelist row", vim.log.levels.INFO)
      return
    end
    M.render_right()
  end, opts)

  vim.keymap.set("n", "s", function()
    local item = get_current_item()
    if not item or item.kind ~= "changelist" then
      vim.notify("UCore: move to a changelist row", vim.log.levels.INFO)
      return
    end
    local confirm = vim.fn.confirm(
      "UCore: submit changelist " .. tostring(item.number) .. "?", "&Submit\n&Cancel", 2, "Question"
    )
    if confirm ~= 1 then return end
    local ok, err = p4.submit_changelist(item.number)
    if ok then
      vim.notify("UCore: submit successful", vim.log.levels.INFO)
      M.refresh()
    else
      vim.notify("UCore: submit failed:\n" .. tostring(err), vim.log.levels.ERROR)
    end
  end, opts)

  vim.keymap.set("n", "R", function()
    M.refresh()
  end, opts)

  vim.keymap.set("n", "?", function()
    vim.notify([[
UCore VCS Dashboard

j/k      Move selection
Space    Toggle file checked
Enter    Open file / show changelist detail
d        Refresh preview pane (diff)
c        p4 edit selected file
a        p4 add selected local candidate
r        Revert selected file (with confirmation)
m        Open commit UI with checked files
l        Show changelist detail in preview
s        Submit selected changelist
R        Refresh data
?        This help
q/Esc    Close dashboard
]], vim.log.levels.INFO)
  end, opts)

  local all_bufs = { state.wins.header_buf, state.wins.left_buf, state.wins.right_buf, state.wins.footer_buf }
  for _, b in ipairs(all_bufs) do
    vim.keymap.set("n", "q", M.close, { buffer = b, nowait = true, silent = true })
    vim.keymap.set("n", "<Esc>", M.close, { buffer = b, nowait = true, silent = true })
  end

  if autocmd_group then
    pcall(vim.api.nvim_del_augroup_by_id, autocmd_group)
  end
  autocmd_group = vim.api.nvim_create_augroup("UCoreVcsDashboard", { clear = true })
  for _, b in ipairs(all_bufs) do
    vim.api.nvim_create_autocmd("BufWinLeave", {
      group = autocmd_group,
      buffer = b,
      once = true,
      callback = function()
        vim.schedule(function()
          M.close()
        end)
      end,
    })
  end
end

function M.refresh()
  if not state then return end
  local root = state.root
  local new_data = M.collect_data(root, state.filter)
  state.rows = new_data.rows
  state.info = new_data.info
  state.project_name = new_data.project_name
  if not is_selectable(state.rows[state.cursor]) then
    cursor_to_first_selectable()
  end
  M.render_header()
  M.render_left()
  M.render_right()
  M.render_footer()
end

function M.open(opts)
  opts = opts or {}

  if state then
    local next_filter = opts.filter or state.filter or "all"
    if next_filter ~= state.filter then
      state.cursor = 1
    end
    state.filter = next_filter
    M.refresh()
    return
  end

  local root = opts.root or project.find_project_root()
  if not root then
    vim.notify("UCore: no Unreal project detected", vim.log.levels.ERROR)
    return
  end

  if not p4.detect(root) then
    vim.notify("UCore: no P4 provider detected", vim.log.levels.WARN)
    return
  end

  state = {}
  local wins = open_windows()
  if not wins then state = nil; return end
  state.wins = wins

  state.filter = opts.filter or "all"
  state.data = M.collect_data(root, state.filter)
  state.root = root
  state.rows = state.data.rows
  state.info = state.data.info
  state.project_name = state.data.project_name
  state.cursor = 1

  cursor_to_first_selectable()

  M.render_header()
  M.render_left()
  M.render_right()
  M.render_footer()
  setup_keymaps()
end

return M
