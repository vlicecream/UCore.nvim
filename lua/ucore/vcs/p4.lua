local config = require("ucore.config")

local M = {}

local function sanitize(path)
  if not path then return "" end
  return tostring(path):gsub("\0", "")
end

local function win_path(path)
  return (sanitize(path):gsub("/", "\\"))
end

local function trace_path_event(op, path, root, reason)
  local dir = (config.values and config.values.cache_dir) or (vim.fn.stdpath("data") .. "/ucore")
  pcall(vim.fn.mkdir, dir, "p")

  local lines = {
    string.rep("=", 80),
    os.date("%Y-%m-%d %H:%M:%S") .. "  " .. tostring(op),
    "reason: " .. tostring(reason or ""),
    "path: " .. vim.inspect(path),
    "root: " .. vim.inspect(root),
    "cwd: " .. vim.inspect(vim.fn.getcwd()),
    "stack:",
  }
  vim.list_extend(lines, vim.split(debug.traceback("", 3), "\n", { plain = true }))
  table.insert(lines, "")

  pcall(vim.fn.writefile, lines, dir .. "/vcs-trace.log", "a")
end

local function command_has_suspicious_arg(cmd)
  for _, arg in ipairs(cmd or {}) do
    local value = sanitize(arg)
    if value == "0" or value:match("[/\\]0$") or value:match("^%a+://") then
      return true
    end
  end
  return false
end

local function command_output_is_suspicious(stdout, stderr)
  local text = tostring(stdout or "") .. "\n" .. tostring(stderr or "")
  return text:find("not under client's root", 1, true) ~= nil
      or text:find("Path '", 1, true) ~= nil and text:find("\\0'", 1, true) ~= nil
      or text:find("Path '", 1, true) ~= nil and text:find("/0'", 1, true) ~= nil
end

local function trace_command_event(op, cmd, stdout, stderr, code, reason)
  local dir = (config.values and config.values.cache_dir) or (vim.fn.stdpath("data") .. "/ucore")
  pcall(vim.fn.mkdir, dir, "p")

  local lines = {
    string.rep("=", 80),
    os.date("%Y-%m-%d %H:%M:%S") .. "  " .. tostring(op),
    "reason: " .. tostring(reason or ""),
    "code: " .. tostring(code),
    "cmd: " .. vim.inspect(cmd),
    "stdout: " .. tostring(stdout or ""),
    "stderr: " .. tostring(stderr or ""),
    "cwd: " .. vim.inspect(vim.fn.getcwd()),
    "stack:",
  }
  vim.list_extend(lines, vim.split(debug.traceback("", 3), "\n", { plain = true }))
  table.insert(lines, "")

  pcall(vim.fn.writefile, lines, dir .. "/vcs-trace.log", "a")
end

local function maybe_trace_command(op, cmd, stdout, stderr, code)
  if command_has_suspicious_arg(cmd) then
    trace_command_event(op, cmd, stdout, stderr, code, "suspicious-command-arg")
  elseif command_output_is_suspicious(stdout, stderr) then
    trace_command_event(op, cmd, stdout, stderr, code, "suspicious-command-output")
  end
end

local function trace_reconcile_event(op, root, cmd, stdout, stderr, code, parsed)
  local dir = (config.values and config.values.cache_dir) or (vim.fn.stdpath("data") .. "/ucore")
  pcall(vim.fn.mkdir, dir, "p")

  local lines = {
    string.rep("=", 80),
    os.date("%Y-%m-%d %H:%M:%S") .. "  " .. tostring(op),
    "root: " .. vim.inspect(root),
    "cmd: " .. vim.inspect(cmd),
    "code: " .. tostring(code),
    "cwd: " .. vim.inspect(vim.fn.getcwd()),
    "-- stdout --",
    tostring(stdout or ""),
    "-- stderr --",
    tostring(stderr or ""),
    "-- parsed --",
    vim.inspect(parsed or {}),
    "",
  }

  pcall(vim.fn.writefile, lines, dir .. "/vcs-reconcile.log", "a")
end

local function is_suspicious_file_arg(path, root)
  path = sanitize(path)
  if path == "" or path == "0" or path:match("[/\\]0$") then
    return true, "zero-or-empty-path"
  end
  if path:match("^%a+://") then
    return true, "uri-path"
  end
  return false, nil
end

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
    cmd[#cmd + 1] = sanitize(a)
  end
  return cmd
end

local function p4_raw_cmd(args)
  local vcs_p4 = (config.values.vcs or {}).p4 or {}
  local cmd = { vcs_p4.command or "p4" }
  for _, a in ipairs(args or {}) do
    cmd[#cmd + 1] = a
  end
  return cmd
end

local function apply_env(opts)
  local env = M.build_env()
  if next(env) == nil then
    return opts
  end

  local merged = vim.deepcopy(vim.env)
  for k, v in pairs(env) do
    merged[k] = v
  end
  opts.env = merged
  return opts
end

local function parse_info(result)
  local info = {}
  for line in tostring(result or ""):gmatch("[^\r\n]+") do
    local key, value = line:match("^(.-):%s*(.*)$")
    if key and value then
      info[key:lower()] = value
    end
  end
  return info
end

local function parse_changes(result)
  local changes = {}
  for line in tostring(result or ""):gmatch("[^\r\n]+") do
    local num = line:match("^Change (%d+)")
    if num then
      local user = line:match(" by (.+)@")
      if user then
        local desc = line:match("'(.-)'$")
        table.insert(changes, {
          number = tonumber(num),
          user = user,
          description = desc ~= nil and desc or "",
        })
      end
    end
  end
  return changes
end

local function root_pathspec(root)
  return (root or "."):gsub("/", "\\") .. "\\..."
end

local function join_path(...)
  return table.concat(vim.tbl_map(function(part)
    return tostring(part or ""):gsub("[/\\]+$", ""):gsub("^[/\\]+", "")
  end, { ... }), "/")
end

local function existing_pathspecs(root)
  if not root or root == "" then
    return { root_pathspec(root) }
  end

  local specs = {}
  local normalized_root = root:gsub("[/\\]+$", "")

  local function add_dir(relative)
    local path = join_path(normalized_root, relative)
    if vim.fn.isdirectory(path) == 1 then
      specs[#specs + 1] = win_path(path) .. "\\..."
    end
  end

  local function add_file(path)
    if vim.fn.filereadable(path) == 1 then
      specs[#specs + 1] = win_path(path)
    end
  end

  add_dir("Source")
  add_dir("Config")
  add_dir("Content")

  for _, uproject in ipairs(vim.fn.glob(normalized_root .. "/*.uproject", false, true)) do
    add_file(uproject)
  end

  local plugins_root = join_path(normalized_root, "Plugins")
  if vim.fn.isdirectory(plugins_root) == 1 then
    for _, plugin_dir in ipairs(vim.fn.glob(plugins_root .. "/*", false, true)) do
      if vim.fn.isdirectory(plugin_dir) == 1 then
        add_dir(plugin_dir:sub(#normalized_root + 2) .. "/Source")
        add_dir(plugin_dir:sub(#normalized_root + 2) .. "/Config")
        add_dir(plugin_dir:sub(#normalized_root + 2) .. "/Content")
        for _, uplugin in ipairs(vim.fn.glob(plugin_dir .. "/*.uplugin", false, true)) do
          add_file(uplugin)
        end
      end
    end
  end

  if #specs == 0 then
    specs[#specs + 1] = root_pathspec(root)
  end

  return specs
end

local function normalize_path(path)
  return tostring(path or ""):gsub("\\", "/")
end

local function is_depot_path(path)
  return type(path) == "string" and path:match("^//") ~= nil
end

local function is_real_local_path(path, root)
  path = tostring(path or "")
  if not path or path == "" or path == "0" or path:match("[/\\]0$") then
    return false
  end
  if path:match("^%a+://") then
    return false
  end
  if path:match("^//") or path:find("//", 1, true) then
    return false
  end
  if path:find(" to add ", 1, true) or path:find(" to edit ", 1, true) then
    return false
  end

  local normalized = normalize_path(path)
  if normalized:match("^%a:/") then
    if not root then
      return vim.fn.filereadable(path) == 1 or vim.fn.isdirectory(path) == 1
    end
    local normalized_root = normalize_path(root):lower():gsub("/+$", "")
    local normalized_path = normalize_path(normalized):lower()
    return normalized_path == normalized_root or normalized_path:sub(1, #normalized_root + 1) == normalized_root .. "/"
  end
  return root ~= nil and not normalized:match("^%.%.")
end

function M.is_project_file(path, root)
  return is_real_local_path(path, root)
end

local RECONCILE_EXCLUDED_DIRS = {
  [".git"] = true,
  [".svn"] = true,
  [".p4"] = true,
  [".vs"] = true,
  [".idea"] = true,
  [".vscode"] = true,
  ["binaries"] = true,
  ["build"] = true,
  ["deriveddatacache"] = true,
  ["intermediate"] = true,
  ["saved"] = true,
}

local RECONCILE_EXCLUDED_EXTS = {
  a = true,
  cache = true,
  db = true,
  dll = true,
  dylib = true,
  exe = true,
  exp = true,
  idb = true,
  ilk = true,
  ipch = true,
  lib = true,
  log = true,
  obj = true,
  o = true,
  pdb = true,
  pch = true,
  so = true,
  sqlite = true,
  suo = true,
  tmp = true,
}

local RECONCILE_EXCLUDED_PATTERNS = {
  "%.sln%.dotsettings%.user$",
  "%.user$",
}

local function relative_project_path(path, root)
  if not root then return nil end
  local normalized_root = normalize_path(root):lower():gsub("/+$", "")
  local normalized_path = normalize_path(path)
  local lowered_path = normalized_path:lower()
  if lowered_path == normalized_root then
    return ""
  end
  if lowered_path:sub(1, #normalized_root + 1) ~= normalized_root .. "/" then
    return nil
  end
  return normalized_path:sub(#normalized_root + 2)
end

local function should_keep_reconcile_file(path, root)
  if not is_real_local_path(path, root) then
    return false
  end

  local relative = relative_project_path(path, root)
  if not relative or relative == "" then
    return false
  end

  local lower = normalize_path(relative):lower()
  for segment in lower:gmatch("[^/]+") do
    if RECONCILE_EXCLUDED_DIRS[segment] then
      return false
    end
  end

  local ext = lower:match("%.([^%.]+)$")
  if ext and RECONCILE_EXCLUDED_EXTS[ext] then
    return false
  end

  for _, pattern in ipairs(RECONCILE_EXCLUDED_PATTERNS) do
    if lower:match(pattern) then
      return false
    end
  end

  return true
end

function M.normalize_local_file(path, root)
  path = sanitize(path)
  if not is_real_local_path(path, root) then
    return nil
  end
  if path:gsub("\\", "/"):match("^%a:/") then
    return path
  end
  if not root then
    return nil
  end
  return (root:gsub("[/\\]+$", "") .. "/" .. path):gsub("/", "\\")
end

local function resolve_local_status_path(path, root)
  path = vim.trim(tostring(path or "")):gsub("#%d+$", "")
  if not is_real_local_path(path, root) then
    return nil
  end
  if path:gsub("\\", "/"):match("^%a:/") then
    return path
  end
  return (root:gsub("[/\\]+$", "") .. "/" .. path):gsub("/", "\\")
end

local function reconcile_action_from_text(text)
  text = tostring(text or ""):lower()
  if text:find("edit", 1, true) then
    return "edit"
  end
  if text:find("add", 1, true) then
    return "add"
  end
  if text:find("delete", 1, true) or text:find("deleted", 1, true) then
    return "delete"
  end
  return "reconcile"
end

local function parse_reconcile_output(result, root)
  local files = {}
  for line in tostring(result or ""):gmatch("[^\r\n]+") do
    local raw = vim.trim(line)
    if raw ~= "" then
      local file_part, action_text = raw:match("^(.-)%s+%-%s+(.+)$")
      local path = file_part or raw
      path = path:gsub("#%d+$", "")

      local local_path
      if is_depot_path(path) then
        local_path = M.depot_to_local(path)
      else
        local_path = resolve_local_status_path(path, root)
      end

      if local_path and should_keep_reconcile_file(local_path, root) then
        local action = reconcile_action_from_text(action_text or raw)
        table.insert(files, {
          path = local_path,
          status = action == "add" and "?" or "m",
          action = action,
          reconcile = true,
          raw = raw,
        })
      end
    end
  end
  return files
end

local function reconcile_preview(root, flags)
  local args = {"-n"}
  for _, flag in ipairs(flags or {}) do
    args[#args + 1] = flag
  end
  vim.list_extend(args, existing_pathspecs(root))

  local cmd = M.p4_cmd("reconcile", args)
  local stdout, stderr, code = M.system_err(cmd)
  local parsed = code == 0 and parse_reconcile_output(stdout, root) or {}
  trace_reconcile_event("p4.reconcile_preview", root, cmd, stdout, stderr, code, parsed)
  return parsed, stdout, stderr, code
end

local function reconcile_preview_async(root, flags, cb)
  local args = {"-n"}
  for _, flag in ipairs(flags or {}) do
    args[#args + 1] = flag
  end
  vim.list_extend(args, existing_pathspecs(root))

  local cmd = M.p4_cmd("reconcile", args)
  M.system_async(cmd, nil, function(stdout, stderr, code)
    local parsed = code == 0 and parse_reconcile_output(stdout, root) or {}
    trace_reconcile_event("p4.reconcile_async", root, cmd, stdout, stderr, code, parsed)
    cb(parsed, stdout, stderr, code)
  end)
end

local function async_result(cmd, cb)
  return function(result)
    vim.schedule(function()
      local stdout = result.stdout or ""
      local stderr = result.stderr or ""
      local code = result.code or 0
      maybe_trace_command("p4.system_async", cmd, stdout, stderr, code)
      cb(stdout, stderr, code)
    end)
  end
end

function M.system_async(cmd, stdin, cb)
  local opts = apply_env({ text = true })
  if stdin then
    opts.stdin = stdin
  end
  vim.system(cmd, opts, async_result(cmd, cb))
end

function M.system(cmd)
  local env = M.build_env()
  if next(env) == nil then
    local result = vim.fn.system(cmd)
    maybe_trace_command("p4.system", cmd, result, "", vim.v.shell_error)
    return result
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
  maybe_trace_command("p4.system", cmd, result, "", vim.v.shell_error)
  return result
end

function M.system_err(cmd, stdin)
  local opts = { text = true }
  if stdin then opts.stdin = stdin end
  local env = M.build_env()
  if next(env) == nil then
    local r = vim.system(cmd, opts):wait()
    maybe_trace_command("p4.system_err", cmd, r.stdout or "", r.stderr or "", r.code)
    return r.stdout or "", r.stderr or "", r.code
  end
  local merged = vim.deepcopy(vim.env)
  for k, v in pairs(env) do
    merged[k] = v
  end
  opts.env = merged
  local r = vim.system(cmd, opts):wait()
  maybe_trace_command("p4.system_err", cmd, r.stdout or "", r.stderr or "", r.code)
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
  return parse_info(result), nil
end

function M.client_root()
  local info, err = M.info()
  if not info then
    return nil, err
  end
  return info["client root"], nil
end

function M.is_opened(path)
  path = sanitize(path)
  local result = M.system(M.p4_cmd("opened", {win_path(path)}))
  return vim.v.shell_error == 0 and result ~= ""
end

function M.opened(root)
  local args = {}
  if root then
    args[#args + 1] = root_pathspec(root)
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
      if local_path and is_real_local_path(local_path, root) then
        local change = line:match("%-%s+%S+%s+(%S+)%s+change") or "default"
        if change == "0" then change = "default" end
        table.insert(files, {
          path = local_path,
          action = action,
          depot = depot_file,
          change = change,
        })
      end
    end
  end
  return files
end

function M.status(root)
  local files, _stdout, _stderr, code = reconcile_preview(root, {"-a", "-d"})
  if code ~= 0 then
    return {}
  end
  return files
end

function M.checkout(path, root)
  path = sanitize(path)
  local suspicious, reason = is_suspicious_file_arg(path, root)
  if suspicious then
    trace_path_event("p4.checkout", path, root, reason)
    return true, nil
  end
  if vim.fn.filereadable(path) ~= 1 then
    return false, "file not found: " .. path
  end
  local stdout, stderr, code = M.system_err(M.p4_cmd("edit", {win_path(path)}))
  if code ~= 0 then
    local msg = (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 edit failed"
    return false, msg
  end
  return true, nil
end

function M.diff(path, root)
  path = sanitize(path)
  local suspicious, reason = is_suspicious_file_arg(path, root)
  if suspicious then
    trace_path_event("p4.diff", path, root, reason)
    return "", nil
  end
  if not is_real_local_path(path, root) then
    trace_path_event("p4.diff", path, root, "invalid-local-file-path")
    return nil, "invalid local file path: " .. tostring(path)
  end
  local result = M.system(M.p4_cmd("diff", {"-f", "-du", win_path(path)}))
  if vim.v.shell_error ~= 0 then
    return nil, "p4 diff failed"
  end
  return result, nil
end

function M.depot_to_local(depot_file)
  if not is_depot_path(depot_file) then
    return nil
  end
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
    vim.fn.system({"attrib", "-R", win_path(path)})
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
  path = sanitize(path)
  local suspicious, reason = is_suspicious_file_arg(path, nil)
  if suspicious then
    trace_path_event("p4.reopen_file", path, nil, reason .. " change=" .. tostring(change_num))
    return true, nil
  end
  local stdout, stderr, code = M.system_err(M.p4_cmd("reopen", {"-c", tostring(change_num), win_path(path)}))
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
  for _, raw_path in ipairs(files or {}) do
    local ok, reopen_err = M.reopen_file(raw_path, change_num)
    if not ok then
      table.insert(reopen_errs, vim.fn.fnamemodify(tostring(raw_path or "?"), ":t") .. ": " .. tostring(reopen_err))
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

function M.do_revert(path, root)
  path = sanitize(path)
  local suspicious, reason = is_suspicious_file_arg(path, root)
  if suspicious then
    trace_path_event("p4.do_revert", path, root, reason)
    return true, nil
  end
  if vim.fn.filereadable(path) ~= 1 then
    return false, "file not found: " .. path
  end
  local stdout, stderr, code = M.system_err(M.p4_cmd("revert", {win_path(path)}))
  if code ~= 0 then
    return false, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 revert failed"
  end
  return true, nil
end

function M.add_file(path, root)
  path = sanitize(path)
  local suspicious, reason = is_suspicious_file_arg(path, root)
  if suspicious then
    trace_path_event("p4.add_file", path, root, reason)
    return true, nil
  end
  if not is_real_local_path(path, root) then
    trace_path_event("p4.add_file", path, root, "invalid-local-file-path")
    return false, "invalid local file path: " .. path
  end
  local stdout, stderr, code = M.system_err(M.p4_cmd("add", {win_path(path)}))
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
  local info = M.info()
  local user = info and info["user name"]
  local args = user and {"-s", "shelved", "-u", user} or {"-s", "shelved"}
  local result = M.system(M.p4_cmd("changes", args))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local changes = parse_changes(result)
  if #changes == 0 and user then
    result = M.system(M.p4_cmd("changes", {"-s", "shelved"}))
    if vim.v.shell_error == 0 then
      changes = parse_changes(result)
    end
  end
  return changes
end

function M.pending_changelists(root)
  local result = M.system(M.p4_cmd("changes", {"-s", "pending", root_pathspec(root)}))
  if vim.v.shell_error ~= 0 then
    return {}
  end
  return parse_changes(result)
end

local function allwrite_enabled()
  local result = M.system(M.p4_cmd("client", {"-o"}))
  if vim.v.shell_error ~= 0 then return false end
  return result:match("allwrite") and not result:match("noallwrite")
end

function M.writable_unopened(root)
  if not root then return {} end
  if allwrite_enabled() then return {} end
  local files, _stdout, _stderr, code = reconcile_preview(root, {"-e"})
  if code ~= 0 then return {} end

  local writable = {}
  for _, file in ipairs(files or {}) do
    if file.action == "edit" then
      table.insert(writable, {
        path = file.path,
        status = "writable?",
        action = "writable?",
        raw = file.raw,
      })
    end
  end
  return writable
end

function M.writable_unopened_async(root, cb)
  reconcile_preview_async(root, {"-e"}, function(files, stdout, stderr, code)
    if code ~= 0 then
      cb({}, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 reconcile edit failed")
      return
    end

    local writable = {}
    for _, file in ipairs(files or {}) do
      if file.action == "edit" then
        table.insert(writable, {
          path = file.path,
          status = "writable?",
          action = "writable?",
          raw = file.raw,
        })
      end
    end
    cb(writable, nil)
  end)
end

function M.shelved_detail(change_num)
  local result = M.system(M.p4_cmd("describe", {"-S", tostring(change_num)}))
  if vim.v.shell_error ~= 0 then
    return nil, "failed to describe shelved changelist " .. tostring(change_num)
  end
  local detail = { number = tonumber(change_num), user = "", description = "", files = {}, status = "shelved" }
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
  end
  return detail, nil
end

function M.info_async(cb)
  M.system_async(M.p4_cmd("info", {"-s"}), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb(nil, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 info failed")
      return
    end
    cb(parse_info(stdout), nil)
  end)
end

function M.opened_async(root, cb)
  local path = root and root_pathspec(root) or nil
  local args = {"-F", "%clientFile%|%action%|%depotFile%|%change%", "opened"}
  if path then
    args[#args + 1] = path
  end

  M.system_async(p4_raw_cmd(args), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb({}, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 opened failed")
      return
    end

    local files = {}
    for line in stdout:gmatch("[^\r\n]+") do
      local client_file, action, depot, change = line:match("^(.-)|([^|]+)|([^|]*)|(.*)$")
      if not client_file then
        local depot_rev, opened_action = line:match("^(%S+)%s*%-%s*(%S+)")
        if depot_rev and opened_action then
          depot = depot_rev:gsub("#%d+$", "")
          action = opened_action
          change = line:match("%-%s+%S+%s+(%S+)%s+change") or "default"
        end
      end
      if (not client_file or not is_real_local_path(client_file, root)) and is_depot_path(depot) then
        client_file = M.depot_to_local(depot)
      end
      if client_file and action and is_real_local_path(client_file, root) then
        if not change or change == "" or change == "0" then
          change = "default"
        end
        table.insert(files, {
          path = client_file,
          action = action,
          depot = depot,
          change = change ~= "" and change or "default",
        })
      end
    end
    cb(files, nil)
  end)
end

function M.status_async(root, cb)
  reconcile_preview_async(root, {"-a", "-d"}, function(parsed, stdout, stderr, code)
    if code ~= 0 then
      cb({}, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 reconcile failed")
      return
    end

    cb(parsed, nil)
  end)
end

function M.pending_changelists_async(root, cb)
  local args = {"-s", "pending", root_pathspec(root)}
  M.system_async(M.p4_cmd("changes", args), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb({}, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 pending changes failed")
      return
    end
    cb(parse_changes(stdout), nil)
  end)
end

function M.shelved_changelists_async(root, cb)
  M.info_async(function(info, err)
    local user = info and info["user name"]
    local args = user and {"-s", "shelved", "-u", user} or {"-s", "shelved"}
    M.system_async(M.p4_cmd("changes", args), nil, function(stdout, stderr, code)
      if code ~= 0 then
        cb({}, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 shelved changes failed")
        return
      end
      local changes = parse_changes(stdout)
      if #changes == 0 and user then
        M.system_async(M.p4_cmd("changes", {"-s", "shelved"}), nil, function(stdout2, stderr2, code2)
          if code2 == 0 then
            cb(parse_changes(stdout2), nil)
          else
            cb(changes, nil)
          end
        end)
      else
        cb(changes, nil)
      end
    end)
  end)
end

function M.diff_async(path, root, cb)
  local raw_path = path
  if type(root) == "function" then
    local old_cb = root
    vim.schedule(function()
      old_cb(nil, "internal error: p4.diff_async requires project root")
    end)
    return
  end
  local suspicious, reason = is_suspicious_file_arg(path, root)
  if suspicious then
    trace_path_event("p4.diff_async", path, root, reason)
    vim.schedule(function()
      cb("", nil)
    end)
    return
  end
  path = M.normalize_local_file(path, root)
  if not path then
    trace_path_event("p4.diff_async", raw_path, root, "normalize-local-file-failed")
    vim.schedule(function()
      cb(nil, nil)
    end)
    return
  end
  path = win_path(path)
  M.system_async(M.p4_cmd("diff", {"-f", "-du", path}), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb(nil, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or "p4 diff failed")
      return
    end
    cb(stdout, nil)
  end)
end

function M.changelist_detail_async(change_num, cb)
  M.system_async(M.p4_cmd("describe", {"-s", tostring(change_num)}), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb(nil, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or ("failed to describe changelist " .. tostring(change_num)))
      return
    end

    local detail = { number = tonumber(change_num), user = "", description = "", files = {}, status = "" }
    for line in stdout:gmatch("[^\r\n]+") do
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
    cb(detail, nil)
  end)
end

function M.shelved_detail_async(change_num, cb)
  M.system_async(M.p4_cmd("describe", {"-S", tostring(change_num)}), nil, function(stdout, stderr, code)
    if code ~= 0 then
      cb(nil, (stderr ~= "" and stderr or stdout):match("[^\r\n]+") or ("failed to describe shelved changelist " .. tostring(change_num)))
      return
    end

    local detail = { number = tonumber(change_num), user = "", description = "", files = {}, status = "shelved" }
    for line in stdout:gmatch("[^\r\n]+") do
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
    end
    cb(detail, nil)
  end)
end

return M
