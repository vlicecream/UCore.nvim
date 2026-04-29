local project = require("ucore.project")
local vcs = require("ucore.vcs")

local M = {}

local commit_state = nil

local SEP = string.rep("━", 60)

function M.open(root, opts)
  opts = opts or {}
  if not root then
    root = project.find_project_root()
    if not root then
      vim.notify("UCore: no Unreal project detected", vim.log.levels.ERROR)
      return
    end
  end

  local provider = vcs.detect(root)
  if not provider then
    vim.notify("UCore: no VCS provider detected for this project", vim.log.levels.WARN)
    return
  end

  local files
  if opts.files then
    files = M.build_files_from_paths(provider, root, opts.files)
  else
    files = M.collect_commit_files(provider, root)
  end

  if not files or #files == 0 then
    vim.notify("UCore: no changes to commit", vim.log.levels.INFO)
    return
  end

  local lines = M.build_buffer_lines(provider, root, files)
  local buf = M.create_buffer(lines, provider, root, files)

  commit_state = {
    buf = buf,
    root = root,
    provider = provider,
    files = files,
    file_start = 11,
    file_end = 10 + #files,
    message_start = 10 + #files + 2,
    separator_line = 10 + #files + 1,
  }

  M.setup_keymaps(buf)
  vim.api.nvim_set_current_buf(buf)
end

function M.build_files_from_paths(provider, root, paths)
  local local_path = root:gsub("/", "\\") .. "\\"
  local files = {}
  local seen = {}
  for _, path in ipairs(paths or {}) do
    local rel = path:lower():gsub(local_path:lower(), "")
    local key = rel:lower()
    if not seen[key] then
      seen[key] = true
      table.insert(files, {
        path = path,
        rel = rel,
        status = "edit",
        checked = true,
      })
    end
  end
  return files
end

function M.collect_commit_files(provider, root)
  local local_path = root:gsub("/", "\\") .. "\\"

  if provider.name() == "p4" then
    local opened = provider.opened(root)
    local local_changes = provider.status(root)

    local seen = {}
    local files = {}

    for _, f in ipairs(opened or {}) do
      local rel = f.path:lower():gsub(local_path:lower(), "")
      local key = rel:lower()
      if not seen[key] then
        seen[key] = true
        table.insert(files, {
          path = f.path,
          rel = rel,
          status = f.action,
          checked = true,
          depot = f.depot,
        })
      end
    end

    for _, f in ipairs(local_changes or {}) do
      local rel = f.path:lower():gsub(local_path:lower(), "")
      local key = rel:lower()
      if not seen[key] then
        seen[key] = true
        table.insert(files, {
          path = f.path,
          rel = rel,
          status = f.status == "open for add" and "add" or f.status,
          checked = false,
          is_local = true,
        })
      end
    end

    return files
  end

  local st = provider.status(root)
  local files = {}
  for _, f in ipairs(st or {}) do
    local rel = f.path:lower():gsub(local_path:lower(), "")
    table.insert(files, {
      path = f.path,
      rel = rel,
      status = f.status,
      checked = true,
    })
  end
  return files
end

function M.build_buffer_lines(provider, root, files)
  local proj_name = vim.fn.fnamemodify(root, ":t")
  local lines = {
    "UCore Commit",
    SEP,
    "VCS: " .. provider.name():upper(),
    "Project: " .. proj_name,
    "Root: " .. root,
  }

  if provider.name() == "p4" then
    local info, _ = provider.info(root)
    if info then
      table.insert(lines, "Client: " .. tostring(info["client name"] or "?"))
      table.insert(lines, "User: " .. tostring(info["user name"] or "?"))
    end
  end

  table.insert(lines, "")
  table.insert(lines, "Files:")

  for _, f in ipairs(files) do
    local mark = f.checked and "[x]" or "[ ]"
    local tag = f.is_local and "local" or "opened"
    table.insert(lines, string.format("  %s  %-6s %-6s %s", mark, tag, f.status, f.rel))
  end

  table.insert(lines, "")
  table.insert(lines, "Message:")
  table.insert(lines, "")
  table.insert(lines, SEP)
  table.insert(lines, "<Tab> toggle   <C-s> submit   d diff   a add   r revert   q close")

  return lines
end

function M.create_buffer(lines, provider, root, files)
  vim.cmd("botright 20new")
  local buf = vim.api.nvim_get_current_buf()

  vim.bo[buf].buftype = "acwrite"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "ucore-commit"
  vim.bo[buf].modified = false
  pcall(vim.api.nvim_buf_set_name, buf, "ucore://commit/" .. tostring(buf))

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modified = false

  vim.api.nvim_set_option_value("cursorline", true, { buf = buf })
  vim.api.nvim_win_set_cursor(0, { 1, 1 })

  return buf
end

function M.get_file_at_line(buf, line)
  if not commit_state then return nil end
  local idx = line - commit_state.file_start + 1
  if idx < 1 or idx > #commit_state.files then return nil end
  return commit_state.files[idx], idx
end

function M.is_file_line(buf, line)
  if not commit_state then return false end
  local s = commit_state.file_start
  local e = commit_state.file_end
  return line >= s and line <= e
end

function M.is_message_line(buf, line)
  if not commit_state then return false end
  return line >= (commit_state.message_start + 1)
      and line < commit_state.separator_line
end

function M.toggle_file(buf)
  local cur_line = vim.api.nvim_win_get_cursor(0)[1]
  if not M.is_file_line(buf, cur_line) then
    vim.notify("UCore: move cursor to a file line to toggle", vim.log.levels.INFO)
    return
  end

  local file = M.get_file_at_line(buf, cur_line)
  if not file then return end

  file.checked = not file.checked
  local mark = file.checked and "[x]" or "[ ]"
  local line_content = vim.api.nvim_buf_get_lines(buf, cur_line - 1, cur_line, false)[1] or ""
  local new_line = line_content:gsub("%[.%]", mark)
  vim.api.nvim_buf_set_lines(buf, cur_line - 1, cur_line, false, { new_line })
end

function M.add_file(buf)
  local cur_line = vim.api.nvim_win_get_cursor(0)[1]
  if not M.is_file_line(buf, cur_line) then
    vim.notify("UCore: move cursor to a local file to add", vim.log.levels.INFO)
    return
  end

  local file = M.get_file_at_line(buf, cur_line)
  if not file then return end

  if not file.is_local then
    vim.notify("UCore: file is already opened in P4", vim.log.levels.INFO)
    return
  end

  local provider = commit_state.provider
  if provider.name() == "p4" then
    local ok, err = provider.add_file(file.path)
    if ok then
      file.is_local = false
      file.checked = true
      file.status = "add"
      local new_line = string.format("  [x]  opened  %-6s %s", "add", file.rel)
      vim.api.nvim_buf_set_lines(buf, cur_line - 1, cur_line, false, { new_line })
      vim.notify("UCore: p4 add " .. vim.fn.fnamemodify(file.path, ":t"), vim.log.levels.INFO)
    else
      vim.notify("UCore: p4 add failed: " .. tostring(err), vim.log.levels.ERROR)
    end
  else
    vim.notify("UCore: add is not implemented for " .. provider.name():upper(), vim.log.levels.INFO)
  end
end

function M.get_message(buf)
  if not commit_state then return "" end
  local start = commit_state.message_start + 1
  local end_line = commit_state.separator_line - 1
  local total = vim.api.nvim_buf_line_count(buf)
  end_line = math.min(end_line, total)

  if start > end_line then return "" end

  local msg_lines = vim.api.nvim_buf_get_lines(buf, start - 1, end_line, false)
  local msg = {}
  for _, l in ipairs(msg_lines) do
    table.insert(msg, l)
  end
  return table.concat(msg, "\n"):gsub("^[\r\n]+", ""):gsub("[\r\n]+$", "")
end

function M.get_checked_files(buf)
  if not commit_state then return {} end
  local checked = {}
  for _, f in ipairs(commit_state.files) do
    if f.checked then
      table.insert(checked, f)
    end
  end
  return checked
end

function M.submit(buf)
  if not commit_state then return end

  local message = M.get_message(buf)
  if message == "" then
    vim.notify("UCore: commit message is required", vim.log.levels.WARN)
    return
  end

  local checked = M.get_checked_files(buf)
  if #checked == 0 then
    vim.notify("UCore: no files selected for commit", vim.log.levels.WARN)
    return
  end

  local summary_lines = {"Submit to " .. commit_state.provider.name():upper() .. "?", "", "Files:"}
  for _, f in ipairs(checked) do
    table.insert(summary_lines, "- " .. f.status .. " " .. f.rel)
  end
  table.insert(summary_lines, "")
  table.insert(summary_lines, "Message:")
  local msg_preview = message:gsub("\n", " "):sub(1, 80)
  table.insert(summary_lines, msg_preview)
  table.insert(summary_lines, "")
  table.insert(summary_lines, "Proceed?")

  local confirm = vim.fn.confirm(
    table.concat(summary_lines, "\n"),
    "&Yes\n&No",
    2,
    "Question"
  )
  if confirm ~= 1 then
    return
  end

  vim.notify("UCore: submitting...", vim.log.levels.INFO)

  local file_paths = vim.tbl_map(function(f) return f.path end, checked)
  local ok, err = commit_state.provider.commit(commit_state.root, file_paths, message, {})

  if ok then
    vim.notify("UCore: submit successful", vim.log.levels.INFO)
    vim.api.nvim_buf_delete(buf, { force = true })
    commit_state = nil
  else
    local err_text = tostring(err)
    local change_hint = ""
    local change_num = err_text:match("Change (%d+)")
    if change_num then
      change_hint = "\nChangelist " .. change_num .. " was kept.\nRun :UCore changelists"
    end
    vim.notify("UCore: submit failed:\n" .. err_text .. change_hint, vim.log.levels.ERROR)
    vim.bo[buf].modified = true
  end
end

function M.diff_file(buf)
  local cur_line = vim.api.nvim_win_get_cursor(0)[1]
  if not M.is_file_line(buf, cur_line) then
    vim.notify("UCore: move cursor to a file line to diff", vim.log.levels.INFO)
    return
  end
  local file = M.get_file_at_line(buf, cur_line)
  if not file then return end

  local provider = commit_state.provider
  local diff_text, diff_err = provider.diff(file.path)
  if diff_err then
    vim.notify("UCore: diff failed: " .. tostring(diff_err), vim.log.levels.ERROR)
    return
  end
  if not diff_text or diff_text == "" then
    vim.notify("UCore: no diff for " .. vim.fn.fnamemodify(file.path, ":t"), vim.log.levels.INFO)
    return
  end

  vim.cmd("botright 12new")
  local dbuf = vim.api.nvim_get_current_buf()
  vim.bo[dbuf].buftype = "nofile"
  vim.bo[dbuf].bufhidden = "wipe"
  vim.bo[dbuf].swapfile = false
  vim.bo[dbuf].filetype = "diff"
  pcall(vim.api.nvim_buf_set_name, dbuf, "ucore://diff/" .. vim.fn.fnamemodify(file.path, ":t"))

  local diff_lines = vim.split(diff_text, "\n", { plain = true })
  local header = "--- a/" .. file.rel
  local header2 = "+++ b/" .. file.rel
  table.insert(diff_lines, 1, header2)
  table.insert(diff_lines, 1, header)
  vim.api.nvim_buf_set_lines(dbuf, 0, -1, false, diff_lines)
  vim.bo[dbuf].modified = false
end

function M.revert_file(buf)
  local cur_line = vim.api.nvim_win_get_cursor(0)[1]
  if not M.is_file_line(buf, cur_line) then
    vim.notify("UCore: move cursor to a file line to revert", vim.log.levels.INFO)
    return
  end
  local file = M.get_file_at_line(buf, cur_line)
  if not file then return end

  local confirm = vim.fn.confirm(
    "UCore: revert " .. vim.fn.fnamemodify(file.path, ":t") .. "?\n\nThis discards local changes.",
    "&Revert\n&Cancel",
    2,
    "Question"
  )
  if confirm ~= 1 then return end

  local provider = commit_state.provider
  if provider.name() == "p4" then
    provider.do_revert(file.path)
    vim.notify("UCore: reverted " .. vim.fn.fnamemodify(file.path, ":t"), vim.log.levels.INFO)
  else
    vim.notify("UCore: revert is not implemented for " .. provider.name():upper(), vim.log.levels.INFO)
  end

  vim.api.nvim_buf_delete(buf, { force = true })
  commit_state = nil
end

function M.close(buf)
  vim.api.nvim_buf_delete(buf, { force = true })
  commit_state = nil
end

function M.setup_keymaps(buf)
  local opts = { buffer = buf, nowait = true, silent = true }

  vim.keymap.set("n", "<Tab>", function()
    M.toggle_file(buf)
  end, opts)

  vim.keymap.set("i", "<Tab>", function()
    local cur_line = vim.api.nvim_win_get_cursor(0)[1]
    if M.is_file_line(buf, cur_line) then
      M.toggle_file(buf)
    else
      vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<Tab>", true, false, true), "n", false)
    end
  end, opts)

  vim.keymap.set("n", "<C-s>", function()
    M.submit(buf)
  end, opts)

  vim.keymap.set("i", "<C-s>", function()
    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<Esc>", true, false, true), "n", false)
    M.submit(buf)
  end, opts)

  vim.keymap.set("n", "d", function()
    M.diff_file(buf)
  end, opts)

  vim.keymap.set("n", "a", function()
    M.add_file(buf)
  end, opts)

  vim.keymap.set("n", "r", function()
    M.revert_file(buf)
  end, opts)

  vim.keymap.set("n", "q", function()
    M.close(buf)
  end, opts)
end

return M
