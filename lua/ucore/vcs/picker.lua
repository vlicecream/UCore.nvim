local project = require("ucore.project")
local vcs = require("ucore.vcs")
local p4 = require("ucore.vcs.p4")

local M = {}

local function split_path(path)
  local name = vim.fn.fnamemodify(path, ":t")
  local dir = vim.fn.fnamemodify(path, ":h")
  return name, dir
end

local function collect_items(root, provider, filter)
  filter = filter or "all"
  local items = {}

  local opened = provider.opened(root) or {}
  local local_files = provider.status(root) or {}
  local local_seen = {}
  for _, f in ipairs(opened) do
    local_seen[f.path:lower()] = true
  end
  local unique_locals = vim.tbl_filter(function(f)
    return not local_seen[f.path:lower()]
  end, local_files)

  local changes = {}
  if provider.pending_changelists then
    changes = provider.pending_changelists(root) or {}
  end

  if filter == "all" or filter == "files" then
    for _, f in ipairs(opened) do
      local name, dir = split_path(f.path)
      table.insert(items, {
        kind = "file",
        group = "Opened",
        status = f.action,
        path = f.path,
        filename = name,
        directory = dir,
        checked = true,
        data = f,
      })
    end
    for _, f in ipairs(unique_locals) do
      local name, dir = split_path(f.path)
      local label = (f.status == "open for add" or f.status == "add") and "add?" or "modify?"
      table.insert(items, {
        kind = "file",
        group = "Local",
        status = label,
        path = f.path,
        filename = name,
        directory = dir,
        checked = false,
        is_local = true,
        data = f,
      })
    end
  end

  if filter == "all" or filter == "changelists" then
    for _, ch in ipairs(changes) do
      table.insert(items, {
        kind = "changelist",
        group = "Changelist",
        change = ch.number,
        description = ch.description:gsub("\n", " "):sub(1, 60),
        data = ch,
      })
    end
  end

  if filter == "all" then
    table.insert(items, {
      kind = "action",
      group = "Action",
      name = "Submit checked files",
      description = "Open commit UI with all checked files",
      action = "commit",
    })
    table.insert(items, {
      kind = "action",
      group = "Action",
      name = "Refresh dashboard",
      description = "Re-fetch P4 data",
      action = "refresh",
    })
  end

  return items
end

local function make_display(item)
  if item.kind == "file" then
    return string.format("[%s]  %-7s %-30s %s",
      item.group, item.status, item.filename, item.directory)
  elseif item.kind == "changelist" then
    return string.format("[%s]  %-5d  %s",
      item.group, item.change, item.description)
  elseif item.kind == "action" then
    return string.format("[%s]  %s  %s",
      item.group, item.name, item.description)
  end
  return ""
end

local function make_ordinal(item)
  local parts = {}
  table.insert(parts, item.group or "")
  if item.kind == "file" then
    table.insert(parts, item.status or "")
    table.insert(parts, item.filename or "")
    table.insert(parts, item.directory or "")
    table.insert(parts, item.path or "")
  elseif item.kind == "changelist" then
    table.insert(parts, tostring(item.change))
    table.insert(parts, item.description or "")
  elseif item.kind == "action" then
    table.insert(parts, item.name or "")
    table.insert(parts, item.description or "")
  end
  return table.concat(parts, " ")
end

local function setup_preview(entry, bufnr)
  local item = entry.value
  if not item then return end

  if item.kind == "file" then
    local diff_text, diff_err = p4.diff(item.path)
    if diff_text and diff_text ~= "" then
      local lines = vim.split(diff_text, "\n", { plain = true })
      table.insert(lines, 1, "+++ b/" .. (item.directory .. "/" .. item.filename):gsub("\\", "/"))
      table.insert(lines, 1, "--- a/" .. (item.directory .. "/" .. item.filename):gsub("\\", "/"))
      vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
      vim.bo[bufnr].filetype = "diff"
      return
    end
    if item.path and vim.fn.filereadable(item.path) == 1 then
      local ok, lines = pcall(vim.fn.readfile, item.path, "", 500)
      if ok then
        vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
        vim.bo[bufnr].filetype = vim.filetype.match({ filename = item.path }) or ""
        return
      end
    end
  elseif item.kind == "changelist" then
    local detail, err = p4.changelist_detail(item.change)
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
        table.insert(lines, "  " .. f.status .. " " .. f.path)
      end
      vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
      vim.bo[bufnr].filetype = "text"
      return
    end
  elseif item.kind == "action" then
    local lines
    if item.action == "commit" then
      lines = {
        "Submit checked files",
        "",
        "Open the commit UI with all currently checked files.",
        "",
        "Key: m",
        "",
        "If no files are checked, all opened files are included.",
        "",
        "Steps:",
        "  1. Review and toggle files with `<Tab>` / `space` (future)",
        "  2. Press `m` to open the commit scratch buffer",
        "  3. Write a message and press `<C-s>` to submit",
      }
    elseif item.action == "refresh" then
      lines = {
        "Refresh dashboard",
        "",
        "Re-fetch opened files, local candidates, and pending changelists.",
        "",
        "Key: R",
      }
    else
      lines = { item.name, "", item.description }
    end
    vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
    vim.bo[bufnr].filetype = "text"
    return
  end

  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, { "(no preview)" })
  vim.bo[bufnr].filetype = "text"
end

function M.open(opts)
  opts = opts or {}
  local filter = opts.filter or "all"

  local root = project.find_project_root()
  if not root then
    vim.notify("UCore: no Unreal project detected", vim.log.levels.ERROR)
    return
  end

  local provider = vcs.detect(root)
  if not provider then
    vim.notify("UCore: no P4 provider detected", vim.log.levels.WARN)
    return
  end

  local items = collect_items(root, provider, filter)
  if #items == 0 then
    vim.notify("UCore VCS: no items to display", vim.log.levels.INFO)
    return
  end

  local telescope_ok = pcall(require, "telescope.pickers")
  if not telescope_ok then
    local text_items = vim.tbl_map(make_display, items)
    local lookup = {}
    for i, item in ipairs(items) do
      lookup[text_items[i]] = item
    end
    vim.ui.select(text_items, {
      prompt = "UCore VCS",
      format_item = function(s) return s end,
    }, function(choice)
      if not choice then return end
      local item = lookup[choice]
      if not item then return end
      if item.kind == "file" and item.path and vim.fn.filereadable(item.path) == 1 then
        vim.cmd.edit(vim.fn.fnameescape(item.path))
      end
    end)
    return
  end

  local pickers = require("telescope.pickers")
  local finders = require("telescope.finders")
  local conf = require("telescope.config").values
  local previewers = require("telescope.previewers")
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")

  local state = {
    root = root,
    provider = provider,
    items = items,
    filter = filter,
  }

  pickers.new({}, {
    prompt_title = "UCore VCS",
    finder = finders.new_table({
      results = items,
      entry_maker = function(item)
        local display = make_display(item)
        return {
          value = item,
          display = display,
          ordinal = make_ordinal(item),
          path = item.kind == "file" and item.path or nil,
          filename = item.filename,
          lnum = 1,
        }
      end,
    }),
    previewer = previewers.new_buffer_previewer({
      define_preview = function(self, entry)
        setup_preview(entry, self.state.bufnr)
      end,
    }),
    sorter = conf.generic_sorter({}),
    attach_mappings = function(prompt_bufnr)

      actions.select_default:replace(function()
        local sel = action_state.get_selected_entry()
        if not sel then return end
        actions.close(prompt_bufnr)
        local item = sel.value
        if item.kind == "file" and item.path and vim.fn.filereadable(item.path) == 1 then
          vim.cmd.edit(vim.fn.fnameescape(item.path))
        elseif item.kind == "changelist" then
          vim.schedule(function()
            require("ucore.vcs.changelists").detail(state.root, state.provider, item.change)
          end)
        elseif item.kind == "action" and item.action == "commit" then
          vim.schedule(function()
            require("ucore.vcs.commit").open(state.root, {})
          end)
        elseif item.kind == "action" and item.action == "refresh" then
          vim.schedule(function()
            M.open(opts)
          end)
        end
      end)

      vim.keymap.set("n", "d", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "file" then
          vim.notify("UCore: move cursor to a file row", vim.log.levels.INFO)
          return
        end
        actions.close(prompt_bufnr)
        local item = sel.value
        local diff_text, diff_err = p4.diff(item.path)
        if diff_err then
          vim.notify("UCore: " .. tostring(diff_err), vim.log.levels.ERROR)
          return
        end
        if not diff_text or diff_text == "" then
          vim.notify("UCore VCS: no diff for " .. item.filename, vim.log.levels.INFO)
          return
        end
        vim.cmd("belowright 12new")
        local dbuf = vim.api.nvim_get_current_buf()
        vim.bo[dbuf].buftype = "nofile"
        vim.bo[dbuf].bufhidden = "wipe"
        vim.bo[dbuf].swapfile = false
        vim.bo[dbuf].filetype = "diff"
        pcall(vim.api.nvim_buf_set_name, dbuf, "ucore://vcs/diff/" .. item.filename)
        local lines = vim.split(diff_text, "\n", { plain = true })
        table.insert(lines, 1, "")
        table.insert(lines, 1, "Diff: " .. (item.directory .. "/" .. item.filename):gsub("\\", "/"))
        table.insert(lines, 1, "")
        vim.api.nvim_buf_set_lines(dbuf, 0, -1, false, lines)
        vim.bo[dbuf].modified = false
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "c", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "file" then
          vim.notify("UCore: move cursor to a file row", vim.log.levels.INFO)
          return
        end
        local item = sel.value
        if item.checked and not item.is_local then
          vim.notify("UCore: " .. item.filename .. " is already opened in P4", vim.log.levels.INFO)
          return
        end
        local ok, err = p4.checkout(item.path)
        if ok then
          item.checked = true
          item.is_local = false
          item.status = "edit"
          vim.notify("UCore: p4 edit " .. item.filename, vim.log.levels.INFO)
        else
          vim.notify("UCore: p4 edit failed: " .. tostring(err), vim.log.levels.ERROR)
        end
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "a", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "file" then
          vim.notify("UCore: move cursor to a local candidate", vim.log.levels.INFO)
          return
        end
        local item = sel.value
        if not item.is_local then
          vim.notify("UCore: " .. item.filename .. " is already opened in P4", vim.log.levels.INFO)
          return
        end
        local ok, err = p4.add_file(item.path)
        if ok then
          item.checked = true
          item.is_local = false
          item.status = "add"
          vim.notify("UCore: p4 add " .. item.filename, vim.log.levels.INFO)
        else
          vim.notify("UCore: p4 add failed: " .. tostring(err), vim.log.levels.ERROR)
        end
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "r", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "file" then
          vim.notify("UCore: move cursor to a file row", vim.log.levels.INFO)
          return
        end
        local item = sel.value
        local confirm = vim.fn.confirm(
          "UCore: revert " .. item.filename .. "?", "&Revert\n&Cancel", 2, "Question"
        )
        if confirm ~= 1 then return end
        actions.close(prompt_bufnr)
        vim.schedule(function()
          p4.do_revert(item.path)
          vim.notify("UCore: reverted " .. item.filename, vim.log.levels.INFO)
        end)
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "m", function()
        local sel = action_state.get_selected_entry()
        actions.close(prompt_bufnr)
        vim.schedule(function()
          if sel and sel.value.kind == "file" then
            require("ucore.vcs.commit").open(state.root, { files = { sel.value.path } })
          else
            require("ucore.vcs.commit").open(state.root, {})
          end
        end)
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "l", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "changelist" then
          vim.notify("UCore: move cursor to a changelist row", vim.log.levels.INFO)
          return
        end
        actions.close(prompt_bufnr)
        vim.schedule(function()
          require("ucore.vcs.changelists").detail(state.root, state.provider, sel.value.change)
        end)
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "R", function()
        actions.close(prompt_bufnr)
        vim.schedule(function()
          M.open(opts)
        end)
      end, { buffer = prompt_bufnr, nowait = true })

      vim.keymap.set("n", "<C-q>", function()
        local sel = action_state.get_selected_entry()
        if not sel or sel.value.kind ~= "file" then
          vim.notify("UCore: move cursor to a file row", vim.log.levels.INFO)
          return
        end
        local qf = {}
        for _, item in ipairs(items) do
          if item.kind == "file" then
            table.insert(qf, {
              filename = item.path,
              text = "[" .. item.group .. "] " .. item.status .. " " .. item.filename,
            })
          end
        end
        if #qf > 0 then
          vim.fn.setqflist(qf)
          vim.cmd("copen")
          vim.notify("UCore: sent " .. #qf .. " file rows to quickfix", vim.log.levels.INFO)
        end
      end, { buffer = prompt_bufnr, nowait = true })

      return true
    end,
  }):find()
end

return M
