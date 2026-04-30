local config = require("ucore.config")
local vcs = require("ucore.vcs")

local M = {}

local group_name = "UCoreReadonlySave"

local function debug_log(event, payload)
  local ok, inspect = pcall(vim.inspect, payload or {})
  local body = ok and inspect or tostring(payload)
  local dir = (config.values.cache_dir or (vim.fn.stdpath("data") .. "/ucore")) .. "/logs"
  local path = dir .. "/readonly-preflight.log"
  pcall(vim.fn.mkdir, dir, "p")
  pcall(vim.fn.writefile, {
    string.rep("=", 80),
    os.date("%Y-%m-%d %H:%M:%S") .. "  " .. event,
    body,
    "",
  }, path, "a")
end

local function refresh_dashboard()
  vim.schedule(function()
    local ok_m, dashboard = pcall(require, "ucore.vcs.dashboard")
    if ok_m and dashboard and dashboard.refresh then
      dashboard.refresh()
    end
  end)
end

local function prompt_readonly_file(path, action_label)
  local fname = vim.fn.fnamemodify(path, ":t")
  return vim.fn.confirm(
    "UCore: read-only P4 file\n\n" .. fname .. "\n\nChoose how to " .. action_label .. ":",
    "&P4 checkout/edit\n&Make writable only\n&Cancel",
    1,
    "Warning"
  )
end

local function apply_readonly_choice(buf, path, choice, project_root, already_opened)
  debug_log("apply_choice", {
    choice = choice,
    path = path,
    project_root = project_root,
    already_opened = already_opened,
  })

  if choice == 1 then
    local p4 = require("ucore.vcs.p4")
    if already_opened then
      p4.make_writable(path)
      debug_log("already_opened_make_writable", { path = path })
      vim.bo[buf].readonly = false
      vim.notify("UCore: file already opened in P4, made writable", vim.log.levels.INFO)
      refresh_dashboard()
      return true
    end

    local ok, err = p4.checkout(path, project_root)
    if ok then
      debug_log("p4_checkout_ok", { path = path })
      vim.bo[buf].readonly = false
      vim.notify("UCore: p4 edit " .. vim.fn.fnamemodify(path, ":t"), vim.log.levels.INFO)
      refresh_dashboard()
      return true
    end
    debug_log("p4_checkout_failed", { path = path, err = err })
    vim.notify("UCore: p4 edit failed: " .. tostring(err), vim.log.levels.ERROR)
    return false
  elseif choice == 2 then
    local p4 = require("ucore.vcs.p4")
    p4.make_writable(path)
    debug_log("make_writable_only", { path = path })
    vim.bo[buf].readonly = false
    vim.notify("UCore: made writable only (not opened in P4)", vim.log.levels.INFO)
    refresh_dashboard()
    return true
  end

  return false
end

local function make_already_opened_writable(buf, path)
  local p4 = require("ucore.vcs.p4")
  p4.make_writable(path)
  debug_log("already_opened_auto_writable", { path = path })
  vim.bo[buf].readonly = false
  vim.notify("UCore: file already checked out, made writable", vim.log.levels.INFO)
  refresh_dashboard()
end

local function should_prompt_for_readonly(buf, path)
  if vim.bo[buf].buftype ~= "" or path == "" then
    debug_log("should_prompt_false", {
      reason = "special-buffer-or-empty-path",
      buf = buf,
      path = path,
      buftype = vim.bo[buf].buftype,
    })
    return false, nil
  end
  if not vim.bo[buf].readonly and vim.fn.filewritable(path) == 1 then
    debug_log("should_prompt_false", {
      reason = "writable-buffer-and-file",
      buf = buf,
      path = path,
      readonly = vim.bo[buf].readonly,
      filewritable = vim.fn.filewritable(path),
    })
    return false, nil
  end

  local project_root = require("ucore.project").find_project_root(path)
  if not project_root then
    debug_log("should_prompt_false", {
      reason = "no-project-root",
      buf = buf,
      path = path,
      readonly = vim.bo[buf].readonly,
      filewritable = vim.fn.filewritable(path),
    })
    return false, nil
  end
  local provider = vcs.detect(project_root)
  if not provider then
    debug_log("should_prompt_false", {
      reason = "no-p4-provider",
      buf = buf,
      path = path,
      project_root = project_root,
      readonly = vim.bo[buf].readonly,
      filewritable = vim.fn.filewritable(path),
    })
    return false, nil
  end

  local already_opened = false
  if provider.is_opened then
    already_opened = provider.is_opened(path)
  end

  debug_log("should_prompt_true", {
    buf = buf,
    path = path,
    project_root = project_root,
    already_opened = already_opened,
    readonly = vim.bo[buf].readonly,
    filewritable = vim.fn.filewritable(path),
  })
  return true, project_root, already_opened
end

local function feed_normal_key(key)
  local term = vim.api.nvim_replace_termcodes(key, true, false, true)
  vim.api.nvim_feedkeys(term, "n", false)
end

function M.setup()
  local vcs_config = config.values.vcs or {}
  if vcs_config.enable == false or vcs_config.prompt_on_readonly_save == false then
    debug_log("setup_skipped", {
      vcs_enable = vcs_config.enable,
      prompt_on_readonly_save = vcs_config.prompt_on_readonly_save,
    })
    return
  end

  debug_log("setup_start", {
    cache_dir = config.values.cache_dir,
    preflight_keymaps = vim.g.ucore_readonly_preflight_keymaps,
  })

  local group = vim.api.nvim_create_augroup(group_name, { clear = true })

  if not vim.g.ucore_readonly_preflight_keymaps then
    vim.g.ucore_readonly_preflight_keymaps = true
    for _, key in ipairs({ "i", "I", "a", "A", "o", "O", "s", "S", "c", "C" }) do
      vim.keymap.set("n", key, function()
        local buf = vim.api.nvim_get_current_buf()
        local path = vim.api.nvim_buf_get_name(buf)
        debug_log("edit_key_pressed", {
          key = key,
          buf = buf,
          path = path,
          mode = vim.api.nvim_get_mode().mode,
          buftype = vim.bo[buf].buftype,
          readonly = vim.bo[buf].readonly,
          modifiable = vim.bo[buf].modifiable,
          modified = vim.bo[buf].modified,
          filewritable = path ~= "" and vim.fn.filewritable(path) or nil,
        })
        local should_prompt, project_root, already_opened = should_prompt_for_readonly(buf, path)
        if not should_prompt then
          debug_log("edit_key_passthrough", { key = key, path = path })
          feed_normal_key(key)
          return
        end

        if already_opened then
          make_already_opened_writable(buf, path)
          feed_normal_key(key)
          return
        end

        debug_log("edit_key_prompt", { key = key, path = path, project_root = project_root })
        local choice = prompt_readonly_file(path, "edit")
        if apply_readonly_choice(buf, path, choice, project_root, already_opened) then
          feed_normal_key(key)
        else
          vim.bo[buf].readonly = true
        end
      end, {
        noremap = true,
        silent = true,
        desc = "UCore readonly edit preflight",
      })
    end
    debug_log("preflight_keymaps_registered", {
      keys = { "i", "I", "a", "A", "o", "O", "s", "S", "c", "C" },
    })
  else
    debug_log("preflight_keymaps_already_registered", {})
  end

  vim.api.nvim_create_autocmd("BufWritePre", {
    group = group,
    pattern = "*",
    callback = function(ev)
      local buf = ev.buf
      local path = vim.api.nvim_buf_get_name(buf)
      debug_log("buf_write_pre", {
        buf = buf,
        path = path,
        buftype = vim.bo[buf].buftype,
        modified = vim.bo[buf].modified,
        readonly = vim.bo[buf].readonly,
        filewritable = path ~= "" and vim.fn.filewritable(path) or nil,
      })

      if vim.bo[buf].buftype ~= "" or path == "" then return end
      if not vim.bo[buf].modified then
        return
      end

      if vim.bo[buf].readonly == false and vim.fn.filewritable(path) == 1 then
        return
      end

      local should_prompt, project_root, already_opened = should_prompt_for_readonly(buf, path)
      if not should_prompt then
        if vim.bo[buf].readonly then
          vim.bo[buf].readonly = false
        end
        return
      end
      if already_opened then
        make_already_opened_writable(buf, path)
        return
      end

      local choice = prompt_readonly_file(path, "save")
      if not apply_readonly_choice(buf, path, choice, project_root, already_opened) then
        vim.notify("UCore: save cancelled, buffer still has unsaved changes", vim.log.levels.WARN)
        error("UCore: save cancelled", 0)
      end
    end,
  })

  local prompted = {}
  vim.api.nvim_create_autocmd("InsertEnter", {
    group = group,
    pattern = "*",
    callback = function(ev)
      local buf = ev.buf
      if prompted[buf] then return end

      local path = vim.api.nvim_buf_get_name(buf)
      debug_log("insert_enter", {
        buf = buf,
        path = path,
        readonly = vim.bo[buf].readonly,
        filewritable = path ~= "" and vim.fn.filewritable(path) or nil,
      })
      local should_prompt, project_root, already_opened = should_prompt_for_readonly(buf, path)
      if not should_prompt then return end

      if already_opened then
        make_already_opened_writable(buf, path)
        return
      end

      prompted[buf] = true
      local choice = prompt_readonly_file(path, "edit")
      if not apply_readonly_choice(buf, path, choice, project_root, already_opened) then
        prompted[buf] = nil
        vim.bo[buf].readonly = true
        vim.schedule(function()
          if vim.api.nvim_get_current_buf() == buf then
            vim.cmd("stopinsert")
          end
        end)
      end
    end,
  })
end

return M
