local M = {}

local function executable(name)
  return vim.fn.executable(name) == 1
end

function M.name()
  return "git"
end

function M.detect(root)
  if not executable("git") then
    return false
  end
  local dot_git = vim.fn.finddir(".git", root .. ";")
  return dot_git ~= ""
end

function M.status(root)
  local result = vim.fn.system({"git", "-C", root, "status", "--porcelain"})
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local files = {}
  for line in result:gmatch("[^\r\n]+") do
    local status = line:match("^(..)")
    local path = line:match("^..%s+(.+)$")
    if status and path then
      path = path:gsub('"', "")
      if vim.fn.has("win32") == 1 then
        path = root .. "/" .. path
      else
        path = root .. "/" .. path
      end
      path = path:gsub("/", "\\")
      table.insert(files, {
        path = path,
        status = status:gsub("%s+", ""),
      })
    end
  end
  return files
end

function M.checkout(path)
  return true, nil
end

function M.diff(path)
  local result = vim.fn.system({"git", "diff", "--", path})
  if vim.v.shell_error ~= 0 then
    return nil, "git diff failed"
  end
  return result, nil
end

return M
