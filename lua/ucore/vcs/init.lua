local config = require("ucore.config")

local M = {}

local providers = {
  p4 = require("ucore.vcs.p4"),
  git = require("ucore.vcs.git"),
  svn = require("ucore.vcs.svn"),
}

local detect_order = { "p4", "git", "svn" }

local detected_cache = {}

function M.detect(root)
  if not root or root == "" then
    return nil
  end
  root = root:gsub("\\", "/")
  if detected_cache[root] ~= nil then
    return detected_cache[root]
  end

  local vcs_config = config.values.vcs or {}
  local requested = (vcs_config.provider or "auto"):lower()

  if requested ~= "auto" then
    local provider = providers[requested]
    if provider and provider.detect(root) then
      detected_cache[root] = provider
      return provider
    end
    detected_cache[root] = nil
    return nil
  end

  for _, name in ipairs(detect_order) do
    local provider = providers[name]
    if provider and provider.detect(root) then
      detected_cache[root] = provider
      return provider
    end
  end

  detected_cache[root] = nil
  return nil
end

function M.clear_cache()
  detected_cache = {}
end

function M.status(root)
  local provider = M.detect(root)
  if not provider then
    return nil, "no VCS provider detected"
  end
  local ok, result = pcall(provider.status, provider, root)
  if not ok then
    return nil, tostring(result)
  end
  return result, nil
end

function M.checkout(path)
  local provider = M.detect_for_path(path)
  if not provider then
    return false, "no VCS provider detected"
  end
  if provider.name() == "p4" then
    return provider.checkout(path)
  end
  return true, nil
end

function M.diff(path)
  local provider = M.detect_for_path(path)
  if not provider then
    return nil, "no VCS provider detected"
  end
  return provider.diff(path)
end

function M.detect_for_path(path)
  if not path or path == "" then
    return nil
  end
  local project = require("ucore.project")
  local root = project.find_project_root(path)
  if not root then
    return nil
  end
  return M.detect(root)
end

function M.is_readonly_p4(path)
  local provider = M.detect_for_path(path)
  if not provider or provider.name() ~= "p4" then
    return false
  end
  if vim.fn.filewritable(path) == 1 then
    return false
  end
  if provider.is_opened(path) then
    return false
  end
  return true
end

function M.collect_changes(root)
  local provider = M.detect(root)
  if not provider then
    return nil
  end

  local items = {}
  local seen = {}

  if provider.name() == "p4" then
    local opened = provider.opened(root)
    for _, file in ipairs(opened or {}) do
      local key = file.path:lower()
      if not seen[key] then
        seen[key] = true
        table.insert(items, {
          path = file.path,
          status = file.action,
          provider = "P4",
          depot = file.depot,
        })
      end
    end

    local local_changes = provider.status(root)
    for _, file in ipairs(local_changes or {}) do
      local key = file.path:lower()
      if not seen[key] then
        seen[key] = true
        table.insert(items, {
          path = file.path,
          status = "local",
          provider = "P4",
        })
      end
    end
  else
    local files = provider.status(root)
    for _, file in ipairs(files or {}) do
      table.insert(items, {
        path = file.path,
        status = file.status,
        provider = provider.name():upper(),
      })
    end
  end

  return items
end

function M.setup()
  local vcs_config = config.values.vcs or {}
  if vcs_config.enable == false then
    return
  end
  if vcs_config.prompt_on_readonly_save ~= false then
    require("ucore.autocmd.readonly").setup()
  end
end

return M
