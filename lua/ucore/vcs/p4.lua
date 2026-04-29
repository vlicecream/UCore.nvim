local M = {}

local function executable(name)
  return vim.fn.executable(name) == 1
end

function M.name()
  return "p4"
end

function M.detect(root)
  if not executable("p4") then
    return false
  end
  local result = vim.fn.system({"p4", "info", "-s"})
  return vim.v.shell_error == 0
end

function M.info(root)
  if not root then
    root = vim.fn.getcwd()
  end
  local result = vim.fn.system({"p4", "info", "-s"})
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
  path = path:gsub("/", "\\")
  local result = vim.fn.system({"p4", "opened", path})
  return vim.v.shell_error == 0 and result ~= ""
end

function M.opened(root)
  local args = {"p4", "opened"}
  if root then
    args[#args + 1] = root:gsub("/", "\\") .. "/..."
  end
  local result = vim.fn.system(args)
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
  local result = vim.fn.system({"p4", "status", "-s", (root or "."):gsub("/", "\\") .. "/..."})
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
  local result = vim.fn.system({"p4", "edit", path:gsub("/", "\\")})
  if vim.v.shell_error ~= 0 then
    local err = result:match("[^\r\n]+") or result
    return false, "p4 edit failed: " .. err
  end
  return true, nil
end

function M.diff(path)
  local result = vim.fn.system({"p4", "diff", path:gsub("/", "\\")})
  if vim.v.shell_error ~= 0 then
    return nil, "p4 diff failed"
  end
  return result, nil
end

function M.depot_to_local(depot_file)
  local result = vim.fn.system({"p4", "where", depot_file})
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

function M.commit(root, files, message)
  return false, "P4 commit via UI is not yet implemented"
end

return M
