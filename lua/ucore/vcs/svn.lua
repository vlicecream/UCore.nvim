local M = {}

local function executable(name)
  return vim.fn.executable(name) == 1
end

function M.name()
  return "svn"
end

function M.detect(root)
  if not executable("svn") then
    return false
  end
  local dot_svn = vim.fn.finddir(".svn", root .. ";")
  return dot_svn ~= ""
end

function M.status(root)
  local result = vim.fn.system({"svn", "status", root})
  if vim.v.shell_error ~= 0 then
    return {}
  end
  local files = {}
  for line in result:gmatch("[^\r\n]+") do
    local status, path = line:match("^(%S+)%s+(.+)$")
    if status and path then
      if not path:match("^%s*$") then
        path = path:gsub("/", "\\")
        table.insert(files, {
          path = vim.fn.fnamemodify(path, ":p"),
          status = status:lower(),
        })
      end
    end
  end
  return files
end

function M.checkout(path)
  return true, nil
end

function M.diff(path)
  local result = vim.fn.system({"svn", "diff", path})
  if vim.v.shell_error ~= 0 then
    return nil, "svn diff failed"
  end
  return result, nil
end

function M.commit(root, files, message)
  return false, "SVN commit via UI is not yet implemented"
end

return M
