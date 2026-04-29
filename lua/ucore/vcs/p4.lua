local config = require("ucore.config")

local M = {}

local function executable(name)
  return vim.fn.executable(name) == 1
end

function M.name()
  return "p4"
end

function M.build_env()
  local vcs_p4 = (config.values.vcs or {}).p4 or {}
  local env = {}

  if vcs_p4.env and type(vcs_p4.env) == "table" then
    for k, v in pairs(vcs_p4.env) do
      env[k] = tostring(v)
    end
  end

  if vcs_p4.port then
    env.P4PORT = tostring(vcs_p4.port)
  end
  if vcs_p4.user then
    env.P4USER = tostring(vcs_p4.user)
  end
  if vcs_p4.client then
    env.P4CLIENT = tostring(vcs_p4.client)
  end
  if vcs_p4.charset then
    env.P4CHARSET = tostring(vcs_p4.charset)
  end
  if vcs_p4.config then
    env.P4CONFIG = tostring(vcs_p4.config)
  end

  return env
end

function M.has_user_overrides()
  local vcs_p4 = (config.values.vcs or {}).p4 or {}
  return vcs_p4.port ~= nil
      or vcs_p4.user ~= nil
      or vcs_p4.client ~= nil
      or vcs_p4.charset ~= nil
      or vcs_p4.config ~= nil
      or (vcs_p4.env ~= nil and next(vcs_p4.env) ~= nil)
end

function M.config_source()
  if M.has_user_overrides() then
    return "user override"
  end
  return "default environment"
end

function M.p4_cmd(subcommand, args)
  local vcs_p4 = (config.values.vcs or {}).p4 or {}
  local cmd = { vcs_p4.command or "p4", subcommand }
  for _, a in ipairs(args or {}) do
    cmd[#cmd + 1] = a
  end
  return cmd
end

function M.system(cmd)
  local env = M.build_env()
  if next(env) == nil then
    return vim.fn.system(cmd)
  end
  local saved = {}
  for k, v in pairs(env) do
    saved[k] = vim.env[k]
    vim.env[k] = v
  end
  local result = vim.fn.system(cmd)
  for k, v in pairs(saved) do
    vim.env[k] = v
  end
  return result
end

function M.system_err(cmd, stdin)
  local opts = { text = true }
  if stdin then opts.stdin = stdin end
  local env = M.build_env()
  if next(env) == nil then
    local r = vim.system(cmd, opts):wait()
    return r.stdout or "", r.stderr or "", r.code
  end
  local merged = vim.deepcopy(vim.env)
  for k, v in pairs(env) do
    merged[k] = v
  end
  opts.env = merged
  local r = vim.system(cmd, opts):wait()
  return r.stdout or "", r.stderr or "", r.code
end

function M.detect(root)
  if not executable(config.values.vcs.p4.command or "p4") then
    return false
  end
  local result = M.system(M.p4_cmd("info", {"-s"}))
  return vim.v.shell_error == 0
end

function M.info(root)
  local result = M.system(M.p4_cmd("info", {"-s"}))
  if vim.v.shell_error ~= 0 then
    return nil, "p4 info failed"
  end
  local info = {}
  for line in result:gmatch("[^\r\n]+") do
    local key, value = line:match("^(.-):%s*(.*)$")
    if key and value then
      info[key:lower()] = value
    end
  end
  return info, nil
end

function M.client_root()
  local info, err = M.info()
  if not info then
    return nil, err
  end
  return info["client root"], nil
end

function M.is_opened(path)
  local result = M.system(M.p4_cmd("opened", {path:gsub("/", "\\")}))
  return vim.v.shell_error == 0 and result ~= ""
end

function M.opened(root)
  local args = {}
  if root then
    args[#args + 1] = root:gsub("/", "\\") .. "/..."
  end
  local result = M.system(M.p4_cmd("opened", args))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local files = {}
  for line in result:gmatch("[^\r\n]+") do
    local depot_rev, action = line:match("^(%S+)%s*%-%s*(%S+)")
    if depot_rev and action then
      local depot_file = depot_rev:gsub("#%d+$", "")
      local local_path = M.depot_to_local(depot_file)
      if local_path then
        table.insert(files, {
          path = local_path,
          action = action,
          depot = depot_file,
        })
      end
    end
  end
  return files
end

function M.status(root)
  local args = {"-s", (root or "."):gsub("/", "\\") .. "/..."}
  local result = M.system(M.p4_cmd("status", args))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local files = {}
  for line in result:gmatch("[^\r\n]+") do
    local status, path = line:match("^(%S+)%s+(.+)$")
    if status and path then
      table.insert(files, {
        path = path,
        status = status:lower(),
      })
    end
  end
  return files
end

function M.checkout(path)
  local stdout, stderr, code = M.system_err(M.p4_cmd("edit", {path:gsub("/", "\\")}))
  if code ~= 0 then
    local msg = (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 edit failed"
    return false, msg
  end
  return true, nil
end

function M.diff(path)
  local result = M.system(M.p4_cmd("diff", {path:gsub("/", "\\")}))
  if vim.v.shell_error ~= 0 then
    return nil, "p4 diff failed"
  end
  return result, nil
end

function M.depot_to_local(depot_file)
  local result = M.system(M.p4_cmd("where", {depot_file}))
  if vim.v.shell_error ~= 0 then
    return nil
  end
  for line in result:gmatch("[^\r\n]+") do
    local parts = vim.split(line, " ")
    if #parts >= 3 then
      return parts[#parts]
    end
  end
  return nil
end

function M.make_writable(path)
  if vim.fn.has("win32") == 1 then
    vim.fn.system({"attrib", "-R", path:gsub("/", "\\")})
  else
    vim.fn.system({"chmod", "u+w", path})
  end
  return vim.v.shell_error == 0
end

function M.create_changelist(description)
  local spec = M.system(M.p4_cmd("changelist", {"-o"}))
  if vim.v.shell_error ~= 0 then
    return nil, "failed to read changelist spec"
  end
  local new_spec = spec:gsub("<enter description here>", description or "(no description)")
  local stdout, stderr, code = M.system_err(M.p4_cmd("changelist", {"-i"}), new_spec)
  if code ~= 0 then
    local msg = (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "failed to create changelist"
    return nil, msg
  end
  local change_num = stdout:match("Change (%d+)")
  if not change_num then
    return nil, "could not parse changelist number"
  end
  return tonumber(change_num), nil
end

function M.reopen_file(path, change_num)
  local stdout, stderr, code = M.system_err(M.p4_cmd("reopen", {"-c", tostring(change_num), path:gsub("/", "\\")}))
  if code ~= 0 then
    return false, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "reopen failed"
  end
  return true, nil
end

function M.submit_changelist(change_num)
  local stdout, stderr, code = M.system_err(M.p4_cmd("submit", {"-c", tostring(change_num)}))
  if code ~= 0 then
    return false, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "submit failed"
  end
  return true, stdout
end

function M.commit(root, files, message, opts)
  local change_num, err = M.create_changelist(message)
  if not change_num then
    return false, "create changelist failed: " .. tostring(err)
  end

  local reopen_errs = {}
  for _, path in ipairs(files or {}) do
    local ok, reopen_err = M.reopen_file(path, change_num)
    if not ok then
      table.insert(reopen_errs, vim.fn.fnamemodify(path, ":t") .. ": " .. tostring(reopen_err))
    end
  end

  if #reopen_errs > 0 then
    local msg = "reopen failed (changelist " .. tostring(change_num) .. " kept):\n" .. table.concat(reopen_errs, "\n")
    msg = msg .. "\n\nRun :UCore vcs and open Pending Changelists"
    return false, msg
  end

  local ok, result = M.submit_changelist(change_num)
  if not ok then
    local msg = "submit failed (changelist " .. tostring(change_num) .. " kept):\n" .. tostring(result)
    msg = msg .. "\n\nRun :UCore vcs and open Pending Changelists"
    return false, msg
  end

  return true, result
end

function M.do_revert(path)
  M.system(M.p4_cmd("revert", {path:gsub("/", "\\")}))
end

function M.add_file(path)
  local stdout, stderr, code = M.system_err(M.p4_cmd("add", {path:gsub("/", "\\")}))
  if code ~= 0 then
    local msg = (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 add failed"
    return false, msg
  end
  return true, nil
end

function M.changelist_detail(change_num)
  local result = M.system(M.p4_cmd("describe", {"-s", tostring(change_num)}))
  if vim.v.shell_error ~= 0 then
    return nil, "failed to describe changelist " .. tostring(change_num)
  end
  local detail = { number = tonumber(change_num), user = "", description = "", files = {}, status = "" }
  for line in result:gmatch("[^\r\n]+") do
    local user = line:match("^User:%s*(.+)$")
    if user then detail.user = user end
    local desc_line = line:match("^Description:%s*(.+)$")
    if desc_line then detail.description = desc_line end
    local cont = line:match("^%s+(.+)$")
    if cont and detail.description ~= "" and detail.files and not cont:match("^Affected") and not cont:match("^Change") then
      local status, path = cont:match("^(%S+)%s+(.+)$")
      if status and path then
        table.insert(detail.files, { status = status, path = path })
      end
    end
    local status_tag = line:match("^Status:%s*(.+)$")
    if status_tag then detail.status = status_tag end
  end
  return detail, nil
end

function M.needs_login()
  local result = M.system(M.p4_cmd("login", {"-s"}))
  return vim.v.shell_error ~= 0
end

function M.login(password)
  local ok = pcall(vim.fn.inputsave)
  local pwd = password
  if not pwd then
    pwd = vim.fn.inputsecret("P4 password: ")
  end
  if ok then pcall(vim.fn.inputrestore) end
  if pwd == "" then
    return false, "password is empty"
  end
  local result = vim.fn.system(M.p4_cmd("login"), pwd)
  if vim.v.shell_error ~= 0 then
    local err = result:match("[^\r\n]+") or "login failed"
    return false, err
  end
  return true, nil
end

function M.shelved_changelists(root)
  local result = M.system(M.p4_cmd("changes", {"-s", "shelved", "-c", (root or "."):gsub("/", "\\") .. "/..."}))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local changes = {}
  for line in result:gmatch("[^\r\n]+") do
    local num, user, desc = line:match("^Change (%d+) .- by (.+) @ .- %((.-)%)")
    if num then
      table.insert(changes, {
        number = tonumber(num),
        user = user,
        description = desc,
      })
    end
  end
  return changes
end

function M.pending_changelists(root)
  local result = M.system(M.p4_cmd("changes", {"-s", "pending", "-c", (root or "."):gsub("/", "\\") .. "/..."}))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local changes = {}
  for line in result:gmatch("[^\r\n]+") do
    local num, user, desc = line:match("^Change (%d+) .- by (.+) @ .- %((.-)%)")
    if num then
      table.insert(changes, {
        number = tonumber(num),
        user = user,
        description = desc,
      })
    end
  end
  return changes
end

M.shelved_detail = M.changelist_detail

return M
