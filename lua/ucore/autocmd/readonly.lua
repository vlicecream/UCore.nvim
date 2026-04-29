local config = require("ucore.config")
local vcs = require("ucore.vcs")

local M = {}

local group_name = "UCoreReadonlySave"
local cancelled_buffers = {}

function M.setup()
  local vcs_config = config.values.vcs or {}
  if vcs_config.enable == false or vcs_config.prompt_on_readonly_save == false then
    return
  end

  local group = vim.api.nvim_create_augroup(group_name, { clear = true })

  vim.api.nvim_create_autocmd("BufWritePre", {
    group = group,
    pattern = "*",
    callback = function(ev)
      local buf = ev.buf
      local path = vim.api.nvim_buf_get_name(buf)

      if vim.bo[buf].buftype ~= "" then
        return
      end
      if path == "" then
        return
      end
      if not vim.bo[buf].modified then
        return
      end

      if vim.bo[buf].readonly == false and vim.fn.filewritable(path) == 1 then
        return
      end

      local project_root = require("ucore.project").find_project_root(path)
      if not project_root then
        return
      end

      if not vcs.is_readonly_p4(path) then
        if vim.bo[buf].readonly then
          vim.bo[buf].readonly = false
        end
        return
      end

      local choice = vim.fn.confirm(
        "UCore: file is read-only\n\"" .. vim.fn.fnamemodify(path, ":t") .. "\"",
        "&P4 checkout/edit\n&Make writable only\n&Cancel save",
        1,
        "Warning"
      )

      if choice == 1 then
        local p4 = require("ucore.vcs.p4")
        local ok, err = p4.checkout(path)
        if ok then
          vim.bo[buf].readonly = false
          vim.bo[buf].modified = true
          vim.notify("UCore: p4 edit " .. vim.fn.fnamemodify(path, ":t"), vim.log.levels.INFO)
        else
          vim.notify("UCore: p4 edit failed: " .. tostring(err), vim.log.levels.ERROR)
          cancelled_buffers[buf] = true
        end
      elseif choice == 2 then
        local p4 = require("ucore.vcs.p4")
        p4.make_writable(path)
        vim.bo[buf].readonly = false
        vim.bo[buf].modified = true
        vim.notify("UCore: made writable (no p4 edit)", vim.log.levels.INFO)
      else
        cancelled_buffers[buf] = true
        vim.notify("UCore: save cancelled, buffer still has unsaved changes", vim.log.levels.WARN)
      end
    end,
  })

  vim.api.nvim_create_autocmd("BufWritePost", {
    group = group,
    pattern = "*",
    callback = function(ev)
      if cancelled_buffers[ev.buf] then
        cancelled_buffers[ev.buf] = nil
        vim.bo[ev.buf].modified = true
      end
    end,
  })
end

return M
