local project = require("ucore.project")
local vcs = require("ucore.vcs")

local M = {}

function M.list(root)
  if not root then
    root = project.find_project_root()
    if not root then
      vim.notify("UCore: no Unreal project detected", vim.log.levels.ERROR)
      return
    end
  end

  local provider = vcs.detect(root)
  if not provider or provider.name() ~= "p4" then
    vim.notify("UCore: changelist management requires P4", vim.log.levels.WARN)
    return
  end

  if not provider.pending_changelists then
    vim.notify("UCore: pending_changelists not available", vim.log.levels.WARN)
    return
  end

  local changes = provider.pending_changelists(root)
  if #changes == 0 then
    vim.notify("UCore: no pending changelists", vim.log.levels.INFO)
    return
  end

  local items = {}
  for _, ch in ipairs(changes) do
    local desc = ch.description:gsub("\n", " "):sub(1, 60)
    table.insert(items, {
      change = ch.number,
      user = ch.user or "?",
      description = desc,
    })
  end

  local ui = require("ucore.ui")
  ui.select.items("UCore pending changelists", items, {
    format_item = function(item)
      return string.format("Change %d  %s  %s", item.change, item.user, item.description)
    end,
    on_choice = function(item)
      if item then
        M.detail(root, provider, item.change)
      end
    end,
  })
end

function M.detail(root, provider, change_num)
  local detail, err = provider.changelist_detail(change_num)
  if not detail then
    vim.notify("UCore: " .. tostring(err), vim.log.levels.ERROR)
    return
  end

  local lines = {
    "Change " .. tostring(detail.number),
    "User: " .. detail.user,
    "Status: " .. detail.status,
    "",
    "Description:",
    "  " .. detail.description,
    "",
    "Files:",
  }

  for _, f in ipairs(detail.files or {}) do
    table.insert(lines, string.format("  %s  %s", f.status, f.path))
  end

  table.insert(lines, "")
  table.insert(lines, string.rep("━", 50))
  table.insert(lines, "Actions (on current buffer):")
  table.insert(lines, "s  submit this changelist")
  table.insert(lines, "q  close")

  vim.cmd("botright 15new")
  local buf = vim.api.nvim_get_current_buf()
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "ucore-changelist"
  vim.bo[buf].modified = false
  pcall(vim.api.nvim_buf_set_name, buf, "ucore://changelist/" .. tostring(detail.number))
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modified = false

  local opts = { buffer = buf, nowait = true, silent = true }
  local state = { buf = buf, root = root, change = detail.number }

  vim.keymap.set("n", "s", function()
    local confirm = vim.fn.confirm(
      "UCore: submit changelist " .. tostring(detail.number) .. "?",
      "&Submit\n&Cancel",
      2,
      "Question"
    )
    if confirm ~= 1 then return end

    vim.notify("UCore: submitting changelist " .. tostring(detail.number) .. "...", vim.log.levels.INFO)
    local ok, err = provider.submit_changelist(detail.number)
    if ok then
      vim.notify("UCore: submit successful", vim.log.levels.INFO)
      vim.api.nvim_buf_delete(buf, { force = true })
    else
      vim.notify("UCore: submit failed:\n" .. tostring(err), vim.log.levels.ERROR)
    end
  end, opts)

  vim.keymap.set("n", "q", function()
    vim.api.nvim_buf_delete(buf, { force = true })
  end, opts)
end

return M
