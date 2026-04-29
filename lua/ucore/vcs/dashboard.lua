local project = require("ucore.project")
local p4 = require("ucore.vcs.p4")

local M = {}

local state = nil
local ns = vim.api.nvim_create_namespace("ucore_vcs_dashboard")
local autocmd_group = nil

local HIGHLIGHTS = {
  UCoreVcsBorder = { fg = "#2aa7ff", bg = "#06121f" },
  UCoreVcsTitle = { fg = "#00d7ff", bold = true },
  UCoreVcsHeader = { link = "NormalFloat" },
  UCoreVcsProvider = { fg = "#7ee787", bold = true },
  UCoreVcsProject = { fg = "#7ee787", bold = true },
  UCoreVcsMeta = { fg = "#ffd166", bold = true },
  UCoreVcsSection = { fg = "#00d7ff", bold = true },
  UCoreVcsSelected = { fg = "#fff2a8", bg = "#5c5200", bold = true },
  UCoreVcsSelector = { fg = "#ffd166", bg = "#5c5200", bold = true },
  UCoreVcsChecked = { fg = "#d7ffaf", bold = true },
  UCoreVcsUnchecked = { link = "NonText" },
  UCoreVcsStatusEdit = { fg = "#4ec9b0" },
  UCoreVcsStatusAdd = { fg = "#6a9955" },
  UCoreVcsStatusDel = { fg = "#f14c4c" },
  UCoreVcsStatusLocal = { fg = "#dcdcaa" },
  UCoreVcsFilename = { link = "Function" },
  UCoreVcsDir = { link = "Comment" },
  UCoreVcsChangelistNum = { link = "Number" },
  UCoreVcsChangelistDesc = { link = "String" },
  UCoreVcsHelp = { link = "NonText" },
  UCoreVcsMuted = { link = "Comment" },
  UCoreVcsFooterKey = { fg = "#ffd166", bold = true },
  UCoreVcsDiffAdd = { fg = "#7ee787" },
  UCoreVcsDiffDel = { fg = "#ff6b6b" },
  UCoreVcsDiffHunk = { fg = "#58a6ff" },
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

local function file_status_label(raw_status)
  if raw_status == "open for add" or raw_status == "add" or raw_status == "a" or raw_status == "?" then
    return "add?"
  end
  return "modify?"
end

local function is_add_candidate(item)
  return item and item.kind == "file" and item.section == "local" and item.status == "add?"
end

local function is_modify_candidate(item)
  return item and item.kind == "file" and item.section == "local" and item.status == "modify?"
end

local function is_dashboard_file(path)
  return p4.is_project_file(path, state and state.root or nil)
end

local function should_show(section)
  if not state then
    return true
  end
  local filter = state.filter or "all"
  if filter == "files" then
    return section == "files"
  end
  if filter == "changelists" or filter == "pending" then
    return section == "pending"
  end
  if filter == "shelved" then
    return section == "shelved"
  end
  return true
end

local function count_values(values)
  return type(values) == "table" and #values or 0
end

local function section_count(section)
  if not state then
    return "0"
  end
  if state.loading[section] then
    return "..."
  end
  if state.data.errors[section] then
    return "err"
  end
  if section == "info" then
    return "1"
  end
  if section == "pending" then
    return tostring(count_values(state.data.pending))
  end
  if section == "shelved" then
    return tostring(count_values(state.data.shelved))
  end
  if section == "files" then
    return tostring(count_values(state.data.opened) + count_values(state.data.local_changes))
  end
  return "0"
end

local function count_section(section)
  return section_count(section)
end

local function default_changelist_count()
  if not state or type(state.data.opened) ~= "table" then
    return 0
  end
  local total = 0
  for _, f in ipairs(state.data.opened) do
    local change = tostring(f.change or "default"):lower()
    if change == "" or change == "default" then
      total = total + 1
    end
  end
  return total
end

local function is_selectable(row)
  return row and (row.kind == "file" or row.kind == "changelist" or row.kind == "shelved")
end

local function loading_message(section)
  if not state or not state.loading[section] then
    return nil
  end
  if section == "info" then
    return "  (workspace loading...)"
  end
  if section == "files" then
    return "  (changes loading...)"
  end
  if section == "pending" then
    return "  (pending changelists loading...)"
  end
  if section == "shelved" then
    return "  (shelved changelists loading...)"
  end
  return "  (loading...)"
end

local function error_message(section)
  if not state then
    return nil
  end
  local err = state.data.errors[section]
  if not err then
    return nil
  end
  return "  (" .. tostring(err) .. ")"
end

local function rebuild_rows()
  if not state then
    return
  end

  local data = state.data
  local rows = {}
  table.insert(rows, { kind = "section", label = "Workspace" })
  table.insert(rows, { kind = "info", label = "Root", value = state.root })

  if state.loading.info then
    table.insert(rows, { kind = "empty", text = loading_message("info") })
  elseif data.errors.info then
    table.insert(rows, { kind = "empty", text = error_message("info") })
  else
    table.insert(rows, { kind = "info", label = "Workspace", value = data.info["client name"] or "?" })
    table.insert(rows, { kind = "info", label = "User", value = data.info["user name"] or "?" })
  end

  if should_show("files") then
    table.insert(rows, { kind = "blank" })
    table.insert(rows, { kind = "section", label = "Changes" })
    if state.loading.files then
      table.insert(rows, { kind = "empty", text = loading_message("files") })
    elseif data.errors.files then
      table.insert(rows, { kind = "empty", text = error_message("files") })
    else
      local local_seen = {}
      for _, f in ipairs(data.opened or {}) do
        if f.path and is_dashboard_file(f.path) then
          local_seen[f.path:lower()] = true
        end
      end

      local has_changes = false
      for _, f in ipairs(data.opened or {}) do
        if is_dashboard_file(f.path) then
          has_changes = true
          local name, dir = split_path(f.path)
          table.insert(rows, {
            kind = "file",
            section = "opened",
            checked = true,
            status = f.action,
            raw_status = f.action,
            path = f.path,
            filename = name,
            directory = dir,
            change = f.change or "default",
          })
        end
      end

      for _, f in ipairs(data.local_changes or {}) do
        if f.path and is_dashboard_file(f.path) and not local_seen[f.path:lower()] then
          has_changes = true
          local name, dir = split_path(f.path)
          table.insert(rows, {
            kind = "file",
            section = "local",
            checked = false,
            status = file_status_label(f.status),
            raw_status = f.status,
            path = f.path,
            filename = name,
            directory = dir,
          })
        end
      end

      if not has_changes then
        table.insert(rows, { kind = "empty", text = "  (no changes)" })
      end
    end
  end

  if should_show("pending") then
    table.insert(rows, { kind = "blank" })
    table.insert(rows, { kind = "section", label = "Pending Changelists" })
    if state.loading.pending then
      table.insert(rows, { kind = "empty", text = loading_message("pending") })
    elseif data.errors.pending then
      table.insert(rows, { kind = "empty", text = error_message("pending") })
    else
      local default_count = default_changelist_count()
      if default_count > 0 then
        table.insert(rows, {
          kind = "empty",
          text = string.format("  default  Default changelist (%d opened files)", default_count),
        })
      end
      if count_values(data.pending) > 0 then
        for _, ch in ipairs(data.pending) do
          table.insert(rows, {
            kind = "changelist",
            number = ch.number,
            description = tostring(ch.description or ""):gsub("\n", " "):sub(1, 60),
            user = ch.user,
          })
        end
      elseif default_count == 0 then
        table.insert(rows, { kind = "empty", text = "  (none)" })
      end
    end
  end

  if should_show("shelved") then
    table.insert(rows, { kind = "blank" })
    table.insert(rows, { kind = "section", label = "Shelved Changelists" })
    if state.loading.shelved then
      table.insert(rows, { kind = "empty", text = loading_message("shelved") })
    elseif data.errors.shelved then
      table.insert(rows, { kind = "empty", text = error_message("shelved") })
    elseif count_values(data.shelved) > 0 then
      for _, ch in ipairs(data.shelved) do
        table.insert(rows, {
          kind = "shelved",
          number = ch.number,
          description = tostring(ch.description or ""):gsub("\n", " "):sub(1, 60),
          user = ch.user,
        })
      end
    else
      table.insert(rows, { kind = "empty", text = "  (none)" })
    end
  end

  state.rows = rows
end

local function cursor_to_first_selectable()
  if not state then return end
  for _, kind in ipairs({ "file", "changelist", "shelved" }) do
    for i, row in ipairs(state.rows) do
      if row.kind == kind then
        state.cursor = i
        return
      end
    end
  end
  state.cursor = 1
end

local function get_current_item()
  if not state then return nil end
  return state.rows[state.cursor]
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
  return vim.o.columns >= 82 and vim.o.lines >= 24
end

local function open_windows()
  if not will_fit() then
    vim.notify("UCore VCS: terminal too small for dashboard", vim.log.levels.WARN)
    return false
  end

  local ed_w = vim.o.columns
  local ed_h = vim.o.lines
  local total_w = ed_w - 4
  local left_w = math.max(42, math.floor(total_w * 0.37))
  local gap = 2
  local right_w = total_w - left_w - gap
  if right_w < 30 then
    left_w = total_w - 34
    right_w = 30
  end

  local h = ed_h - 4
  local row = 1
  local col = math.max(0, math.floor((ed_w - total_w) / 2))
  local header_h = 1
  local footer_h = 1
  local main_row = row + header_h + 2
  local footer_row = row + h - footer_h - 2
  local list_h = math.max(10, footer_row - main_row - 1)

  local success, result = pcall(function()
    local header_buf = vim.api.nvim_create_buf(false, true)
    local header_win = vim.api.nvim_open_win(header_buf, false, {
      relative = "editor",
      width = total_w,
      height = header_h,
      row = row,
      col = col,
      style = "minimal",
      border = "single",
    })
    vim.bo[header_buf].modifiable = true

    local left_buf = vim.api.nvim_create_buf(false, true)
    local left_win = vim.api.nvim_open_win(left_buf, true, {
      relative = "editor",
      width = left_w,
      height = list_h,
      row = main_row,
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
      width = right_w,
      height = list_h,
      row = main_row,
      col = col + left_w + gap,
      style = "minimal",
      border = "single",
    })
    vim.bo[right_buf].modifiable = true

    local footer_buf = vim.api.nvim_create_buf(false, true)
    local footer_win = vim.api.nvim_open_win(footer_buf, false, {
      relative = "editor",
      width = total_w,
      height = footer_h,
      row = footer_row,
      col = col,
      style = "minimal",
      border = "single",
    })
    vim.bo[footer_buf].modifiable = true

    local winhl = "Normal:NormalFloat,FloatBorder:UCoreVcsBorder"
    vim.api.nvim_set_option_value("winhl", winhl, { win = header_win })
    vim.api.nvim_set_option_value("winhl", winhl, { win = left_win })
    vim.api.nvim_set_option_value("winhl", winhl, { win = right_win })
    vim.api.nvim_set_option_value("winhl", winhl, { win = footer_win })

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

local function render_status_text()
  if not state then
    return "closed"
  end
  if state.status and state.status ~= "" then
    return state.status
  end
  if state.loading.info then return "loading workspace..." end
  if state.loading.files then return "loading changes..." end
  if state.loading.pending then return "loading pending..." end
  if state.loading.shelved then return "loading shelved..." end
  return "ready"
end

local function help_line(width)
  local items = {
    "j/k move",
    "Space toggle",
    "Enter open",
    "d diff",
    "c checkout",
    "a add",
    "r revert",
    "m commit",
    "R refresh",
    "q close",
  }
  local short_items = {
    "j/k move",
    "d diff",
    "m commit",
    "R refresh",
    "q close",
  }
  local active = items
  local total = 0
  for _, item in ipairs(active) do
    total = total + vim.fn.strdisplaywidth(item)
  end
  if total + (#active - 1) * 2 > width - 2 then
    active = short_items
    total = 0
    for _, item in ipairs(active) do
      total = total + vim.fn.strdisplaywidth(item)
    end
  end
  local gap = math.max(2, math.floor((width - 2 - total) / math.max(1, #active - 1)))
  return " " .. table.concat(active, string.rep(" ", gap))
end

local function pad_to_width(text, width)
  local pad = width - vim.fn.strdisplaywidth(text)
  if pad <= 0 then
    return text
  end
  return text .. string.rep(" ", pad)
end

local function compose_header(left, center, right, width)
  local left_w = vim.fn.strdisplaywidth(left)
  local center_w = vim.fn.strdisplaywidth(center)
  local right_w = vim.fn.strdisplaywidth(right)
  if left_w + center_w + right_w + 4 > width then
    center = "UCore VCS"
    center_w = vim.fn.strdisplaywidth(center)
  end

  local center_col = math.max(left_w + 2, math.floor((width - center_w) / 2))
  local right_col = math.max(center_col + center_w + 2, width - right_w)
  local line = " " .. left
  line = pad_to_width(line, center_col)
  line = line .. center
  line = pad_to_width(line, right_col)
  line = line .. right
  return line
end

local function add_pattern_highlight(buf, line, text, pattern, group)
  local start_col = text:find(pattern, 1, true)
  if start_col then
    vim.api.nvim_buf_add_highlight(buf, ns, group, line, start_col - 1, start_col - 1 + #pattern)
  end
end

function M.render_header()
  if not state or not state.wins then return end
  local buf = state.wins.header_buf
  vim.bo[buf].modifiable = true
  local info = state.data.info or {}
  local workspace = info["client name"] or "?"
  local user = info["user name"] or "?"
  local left = string.format("P4 | %s", state.project_name or "?")
  local center = "UCore VCS"
  local right = string.format("Workspace: %s | User: %s", workspace, user)
  local width = vim.api.nvim_win_get_width(state.wins.header_win)
  local line = compose_header(left, center, right, width)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { line })
  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  add_pattern_highlight(buf, 0, line, "P4", "UCoreVcsProvider")
  add_pattern_highlight(buf, 0, line, state.project_name or "?", "UCoreVcsProject")
  add_pattern_highlight(buf, 0, line, center, "UCoreVcsTitle")
  add_pattern_highlight(buf, 0, line, "Workspace:", "UCoreVcsMeta")
  add_pattern_highlight(buf, 0, line, "User:", "UCoreVcsMeta")
  vim.bo[buf].modifiable = false
end

function M.render_footer()
  if not state or not state.wins or not state.wins.footer_buf then return end
  local buf = state.wins.footer_buf
  local width = vim.api.nvim_win_get_width(state.wins.footer_win)
  local line = help_line(width)
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { line })
  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsHelp", 0, 0, -1)
  for _, key in ipairs({ "j/k", "Space", "Enter" }) do
    add_pattern_highlight(buf, 0, line, key, "UCoreVcsFooterKey")
  end
  for _, phrase in ipairs({ "d diff", "c checkout", "a add", "r revert", "m commit", "R refresh", "q close" }) do
    local start_col = line:find(phrase, 1, true)
    if start_col then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsFooterKey", 0, start_col - 1, start_col)
    end
  end
  vim.bo[buf].modifiable = false
end

function M.render_left()
  if not state or not state.wins then return end
  local buf = state.wins.left_buf
  local rows = state.rows
  local win_width = vim.api.nvim_win_get_width(state.wins.left_win)

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, {})

  local text_lines = {}
  for i, row in ipairs(rows) do
    local selected = i == state.cursor and is_selectable(row)
    local pointer = selected and "> " or "  "
    if row.kind == "section" then
      local label = row.label
      local suffix = ""
      if label == "Changes" then
        suffix = section_count("files") .. " files"
      elseif label == "Pending Changelists" then
        suffix = section_count("pending") .. " changelists"
      elseif label == "Shelved Changelists" then
        suffix = section_count("shelved") .. " shelves"
      end
      if suffix ~= "" then
        local pad = math.max(2, win_width - vim.fn.strdisplaywidth(label) - vim.fn.strdisplaywidth(suffix) - 2)
        table.insert(text_lines, " " .. label .. string.rep(" ", pad) .. suffix)
      else
        table.insert(text_lines, " " .. label)
      end
    elseif row.kind == "info" then
      table.insert(text_lines, string.format("   %-7s %s", row.label .. ":", row.value))
    elseif row.kind == "blank" then
      table.insert(text_lines, "")
    elseif row.kind == "empty" then
      table.insert(text_lines, row.text or "")
    elseif row.kind == "file" then
      local mark = row.checked and "[x]" or "[ ]"
      local dir = compact_directory(row.directory, 30)
      local change = row.section == "opened" and ("[" .. tostring(row.change or "default") .. "]") or ""
      if dir ~= "" then
        table.insert(text_lines, string.format("%s%s  %-7s %-10s %-24s %s", pointer, mark, row.status, change, row.filename, dir))
      else
        table.insert(text_lines, string.format("%s%s  %-7s %-10s %s", pointer, mark, row.status, change, row.filename))
      end
    elseif row.kind == "changelist" or row.kind == "shelved" then
      local desc = tostring(row.description or ""):gsub("\n", " "):sub(1, 55)
      table.insert(text_lines, string.format("%sCL %-6d  %s", pointer, row.number, desc))
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
    elseif row.kind == "info" or row.kind == "empty" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsMuted", line, 0, #line_text)
    elseif row.kind == "file" then
      if i == state.cursor then
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSelector", line, 0, 2)
      end
      local mark_begin = line_text:find("%[.%]")
      if mark_begin then
        vim.api.nvim_buf_add_highlight(buf, ns, row.checked and "UCoreVcsChecked" or "UCoreVcsUnchecked", line, mark_begin - 1, mark_begin + 2)
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
    elseif row.kind == "changelist" or row.kind == "shelved" then
      if i == state.cursor then
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSelector", line, 0, 2)
      end
      local num = tostring(row.number)
      local num_start = line_text:find(num, 1, true)
      if num_start then
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsChangelistNum", line, num_start - 1, num_start - 1 + #num)
        vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsChangelistDesc", line, num_start + #num + 1, #line_text)
      end
    end
  end

  if is_selectable(state.rows[state.cursor]) then
    local sel_line = state.cursor - 1
    vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSelected", sel_line, 0, -1)
    pcall(vim.api.nvim_win_set_cursor, state.wins.left_win, { sel_line + 1, 0 })
  end
end

local function set_right_lines(lines, ft)
  if not state or not state.wins then return end
  local buf = state.wins.right_buf
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].filetype = ft or "ucore-vcs-detail"
  vim.bo[buf].modifiable = false
  vim.bo[buf].modified = false
  pcall(vim.api.nvim_win_set_cursor, state.wins.right_win, { 1, 0 })
  pcall(vim.api.nvim_set_option_value, "wrap", false, { win = state.wins.right_win })
  pcall(vim.api.nvim_set_option_value, "sidescrolloff", 0, { win = state.wins.right_win })
  pcall(vim.api.nvim_win_call, state.wins.right_win, function()
    vim.fn.winrestview({ topline = 1, lnum = 1, col = 0, curswant = 0, leftcol = 0 })
    vim.cmd("normal! 0")
  end)
  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  for i, line in ipairs(lines) do
    local lnum = i - 1
    if i == 1 then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsSection", lnum, 0, #line)
    elseif line:sub(1, 1) == "+" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsDiffAdd", lnum, 0, #line)
    elseif line:sub(1, 1) == "-" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsDiffDel", lnum, 0, #line)
    elseif line:sub(1, 2) == "@@" then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsDiffHunk", lnum, 0, #line)
    elseif line:match("^%s*File:") or line:match("^%s*Status:") or line:match("^%s*User:") then
      vim.api.nvim_buf_add_highlight(buf, ns, "UCoreVcsMeta", lnum, 0, math.min(#line, 12))
    end
  end
end

local function render_file_summary(item)
  set_right_lines({
    "Diff / Preview",
    "",
    normalize_path(item.path),
    "",
    "Status: " .. tostring(item.status or ""),
    "",
    "Press d to load diff.",
  }, "ucore-vcs-detail")
end

local function render_change_summary(item)
  local label = item.kind == "shelved" and "Shelved Change" or "Change"
  set_right_lines({
    "Diff / Preview",
    "",
    label .. " " .. tostring(item.number),
    "User: " .. tostring(item.user or "?"),
    "",
    "Description:",
    "  " .. tostring(item.description or ""),
    "",
    "Press l or Enter to load detail.",
  }, "ucore-vcs-detail")
end

function M.render_right()
  if not state or not state.wins then return end
  local item = get_current_item()
  if not item then
    set_right_lines({ "Diff / Preview", "", "No selection." }, "ucore-vcs-detail")
    return
  end

  if item.kind == "file" then
    local cached = state.cache.diff[item.path]
    if cached and cached.loading then
      set_right_lines({ "Diff / Preview", "", normalize_path(item.path), "", "Loading diff..." }, "ucore-vcs-detail")
    elseif cached and cached.error then
      set_right_lines({ "Diff / Preview", "", normalize_path(item.path), "", "Diff failed: " .. tostring(cached.error) }, "ucore-vcs-detail")
    elseif cached and cached.text then
      local lines = {
        "Diff / Preview",
        "",
        normalize_path(item.path),
        "Status: " .. tostring(item.status or ""),
        "",
      }
      vim.list_extend(lines, vim.split(cached.text, "\n", { plain = true }))
      set_right_lines(lines, "diff")
    else
      render_file_summary(item)
    end
    return
  end

  if item.kind == "changelist" or item.kind == "shelved" then
    local cache_key = item.kind .. ":" .. tostring(item.number)
    local cached = state.cache.changelist_detail[cache_key]
    if cached and cached.loading then
      set_right_lines({ "Diff / Preview", "", "Change " .. tostring(item.number), "", "Loading detail..." }, "ucore-vcs-detail")
    elseif cached and cached.error then
      set_right_lines({ "Diff / Preview", "", "Change " .. tostring(item.number), "", "Detail failed: " .. tostring(cached.error) }, "ucore-vcs-detail")
    elseif cached and cached.detail then
      local detail = cached.detail
      local lines = {
        "Diff / Preview",
        "",
        (item.kind == "shelved" and "Shelved Change " or "Change ") .. tostring(detail.number),
        "User: " .. tostring(detail.user or "?"),
        "Status: " .. tostring(detail.status or ""),
        "",
        "Description:",
        "  " .. tostring(detail.description or ""),
        "",
        "Files:",
      }
      for _, f in ipairs(detail.files or {}) do
        table.insert(lines, "  " .. tostring(f.status or "") .. "  " .. tostring(f.path or ""))
      end
      set_right_lines(lines, "ucore-vcs-detail")
    else
      render_change_summary(item)
    end
    return
  end

  set_right_lines({ "Diff / Preview", "", "No preview available." }, "ucore-vcs-detail")
end

local function render_all(keep_cursor)
  if not state then return end
  local old_item = get_current_item()
  local old_key = old_item and (old_item.path or (old_item.kind .. ":" .. tostring(old_item.number))) or nil
  rebuild_rows()
  if keep_cursor and old_key then
    for i, row in ipairs(state.rows) do
      local key = row.path or (row.kind .. ":" .. tostring(row.number))
      if key == old_key then
        state.cursor = i
        break
      end
    end
  end
  if not is_selectable(state.rows[state.cursor]) then
    cursor_to_first_selectable()
  end
  M.render_header()
  M.render_left()
  M.render_right()
  M.render_footer()
end

local function load_file_diff(item)
  if not state or not item or not item.path then return end
  if not is_dashboard_file(item.path) then
    state.cache.diff[item.path] = { error = "invalid local file path: " .. tostring(item.path) }
    M.render_right()
    return
  end
  if state.cache.diff[item.path] and state.cache.diff[item.path].text then
    M.render_right()
    return
  end
  state.cache.diff[item.path] = { loading = true }
  M.render_right()
  local token = state.token
  p4.diff_async(item.path, state.root, function(text, err)
    if not state or state.token ~= token then return end
    state.cache.diff[item.path] = err and { error = err } or { text = text or "" }
    M.render_right()
  end)
end

local function load_changelist_detail(item)
  if not state or not item or not item.number then return end
  local cache_key = item.kind .. ":" .. tostring(item.number)
  if state.cache.changelist_detail[cache_key] and state.cache.changelist_detail[cache_key].detail then
    M.render_right()
    return
  end
  state.cache.changelist_detail[cache_key] = { loading = true }
  M.render_right()
  local token = state.token
  local loader = item.kind == "shelved" and p4.shelved_detail_async or p4.changelist_detail_async
  loader(item.number, function(detail, err)
    if not state or state.token ~= token then return end
    state.cache.changelist_detail[cache_key] = err and { error = err } or { detail = detail }
    M.render_right()
  end)
end

local function set_loading_for_filter()
  local filter = state.filter or "all"
  state.loading.info = true
  state.loading.files = filter == "all" or filter == "files"
  state.loading.pending = filter == "all" or filter == "changelists" or filter == "pending"
  state.loading.shelved = filter == "all" or filter == "shelved"
end

local function mark_done(section, err)
  if not state then return end
  state.loading[section] = false
  state.data.errors[section] = err
end

local function is_ready()
  return not state.loading.info
      and not state.loading.files
      and not state.loading.pending
      and not state.loading.shelved
end

local function update_ready_status()
  if state and is_ready() then
    state.status = "ready"
  end
end

function M.load_data()
  if not state then return end
  local root = state.root
  local token = state.token
  set_loading_for_filter()
  state.data.info = {}
  state.data.opened = {}
  state.data.local_changes = {}
  state.data.pending = {}
  state.data.shelved = {}
  state.data.errors = {}
  render_all(false)

  p4.info_async(function(info, err)
    if not state or state.token ~= token then return end
    state.data.info = info or {}
    mark_done("info", err and ("p4 info failed: " .. tostring(err)) or nil)
    update_ready_status()
    render_all(true)
  end)

  if state.loading.files then
    local pending_files = 2
    local file_errors = {}
    local function done_files(kind, err)
      if err then table.insert(file_errors, kind .. ": " .. tostring(err)) end
      pending_files = pending_files - 1
      if pending_files == 0 then
        mark_done("files", #file_errors > 0 and table.concat(file_errors, "; ") or nil)
        update_ready_status()
        render_all(true)
      end
    end

    p4.opened_async(root, function(files, err)
      if not state or state.token ~= token then return end
      state.data.opened = vim.tbl_filter(function(file)
        return file and is_dashboard_file(file.path)
      end, files or {})
      done_files("opened", err)
    end)
    p4.status_async(root, function(files, err)
      if not state or state.token ~= token then return end
      state.data.local_changes = vim.tbl_filter(function(file)
        return file and is_dashboard_file(file.path)
      end, files or {})
      done_files("status", err)
    end)
  end

  if state.loading.pending then
    p4.pending_changelists_async(root, function(changes, err)
      if not state or state.token ~= token then return end
      state.data.pending = changes or {}
      mark_done("pending", err and ("p4 pending failed: " .. tostring(err)) or nil)
      update_ready_status()
      render_all(true)
    end)
  end

  if state.loading.shelved then
    p4.shelved_changelists_async(root, function(changes, err)
      if not state or state.token ~= token then return end
      state.data.shelved = changes or {}
      mark_done("shelved", err and ("p4 shelved failed: " .. tostring(err)) or nil)
      update_ready_status()
      render_all(true)
    end)
  end

  update_ready_status()
  render_all(true)
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
    elseif item.kind == "changelist" or item.kind == "shelved" then
      load_changelist_detail(item)
    end
  end, opts)

  vim.keymap.set("n", "d", function()
    local item = get_current_item()
    if not item or item.kind ~= "file" then
      vim.notify("UCore: move to a file row", vim.log.levels.INFO)
      return
    end
    load_file_diff(item)
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
    local ok, err = p4.checkout(item.path, state.root)
    if ok then
      vim.notify("UCore: p4 edit " .. item.filename, vim.log.levels.INFO)
      M.refresh()
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
    local ok, err = p4.add_file(item.path, state.root)
    if ok then
      vim.notify("UCore: p4 add " .. item.filename, vim.log.levels.INFO)
      M.refresh()
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
    local ok, err = p4.do_revert(item.path, state.root)
    if ok then
      vim.notify("UCore: reverted " .. item.filename, vim.log.levels.INFO)
      M.refresh()
    else
      vim.notify("UCore: revert failed: " .. tostring(err), vim.log.levels.ERROR)
    end
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
    if not item or (item.kind ~= "changelist" and item.kind ~= "shelved") then
      vim.notify("UCore: move to a changelist row", vim.log.levels.INFO)
      return
    end
    load_changelist_detail(item)
  end, opts)

  vim.keymap.set("n", "s", function()
    local item = get_current_item()
    if not item or item.kind ~= "changelist" then
      vim.notify("UCore: move to a pending changelist row", vim.log.levels.INFO)
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
d        Load diff for selected file
c        p4 edit selected file
a        p4 add selected local candidate
r        Revert selected file (with confirmation)
m        Open commit UI with checked files
l        Show changelist detail in preview
s        Submit selected pending changelist
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
  state.token = state.token + 1
  state.status = "refreshing..."
  state.cache.diff = {}
  state.cache.changelist_detail = {}
  M.load_data()
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

  if p4.needs_login() then
    local ok, err = p4.login()
    if not ok then
      vim.notify("UCore: P4 login failed: " .. tostring(err), vim.log.levels.ERROR)
      return
    end
  end

  state = {
    root = root,
    project_name = vim.fn.fnamemodify(root, ":t"),
    filter = opts.filter or "all",
    rows = {},
    cursor = 1,
    status = "loading...",
    token = 1,
    loading = {},
    data = {
      info = {},
      opened = {},
      local_changes = {},
      pending = {},
      shelved = {},
      errors = {},
    },
    cache = {
      diff = {},
      changelist_detail = {},
    },
  }

  local wins = open_windows()
  if not wins then state = nil; return end
  state.wins = wins
  setup_keymaps()
  M.load_data()
end

return M
