local project = require("ucore.project")
local remote = require("ucore.remote")

local M = {}

local FALLBACK_PARENT_CLASSES = {
  "UObject", "AActor", "APawn", "ACharacter",
  "UActorComponent", "USceneComponent", "UUserWidget", "UWidget",
  "UGameInstance", "UGameModeBase", "APlayerController", "APlayerState",
  "AGameStateBase", "USaveGame", "UDataAsset", "UPrimaryDataAsset",
  "UDeveloperSettings", "UBlueprintFunctionLibrary", "UAnimInstance",
  "UAudioComponent",
}

local PARENT_CLASS_CACHE = nil

local DIRECTORY_CHOICES = {
  { label = "Root",  desc = "<Module>/" },
  { label = "Public", desc = "<Module>/Public/" },
  { label = "Private", desc = "<Module>/Private/" },
}

local function ensure_parent_classes(root, callback)
  if PARENT_CLASS_CACHE then
    callback(PARENT_CLASS_CACHE)
    return
  end

  remote.search_symbols(root, "", function(results, err)
    local items = {}
    local seen = {}
    -- Add fallback defaults
    for _, name in ipairs(FALLBACK_PARENT_CLASSES) do
      seen[name] = true
      table.insert(items, name)
    end
    -- Add indexed classes from server
    if not err and results then
      for _, r in ipairs(results) do
        local name = r.name or r.text or ""
        if name:match("^[AU]") and not seen[name] then
          seen[name] = true
          table.insert(items, name)
        end
      end
    end
    table.sort(items)
    PARENT_CLASS_CACHE = items
    callback(items)
  end, 3000)
end

local function detect_module_dir(root, filepath)
  local dir = vim.fn.fnamemodify(filepath, ":p:h")
  while dir and dir ~= root do
    local build_files = vim.fn.glob(dir .. "/*.Build.cs", false, true)
    if #build_files > 0 then
      return dir
    end
    local parent = vim.fn.fnamemodify(dir, ":h")
    if parent == dir then break end
    dir = parent
  end
  return vim.fn.fnamemodify(filepath, ":p:h:h")
end

local function detect_module_api(root, filepath)
  local module_dir = detect_module_dir(root, filepath)
  local module_name = vim.fn.fnamemodify(module_dir, ":t")
  if not module_name or module_name == "" then
    return ""
  end
  return module_name:upper() .. "_API"
end

local function check_name_exists(root, class_name)
  local pattern = root .. "/**/" .. class_name .. ".h"
  local files = vim.fn.glob(pattern, false, true)
  return #files > 0
end

local function prefix_default_parent(class_name)
  if class_name:match("^A") then return "AActor" end
  if class_name:match("^U") then return "UObject" end
  if class_name:match("^F") then return nil end
  if class_name:match("^I") then return nil end
  return "UObject"
end

local function open_files(h_path, cpp_path, class_name)
  vim.schedule(function()
    vim.cmd.edit(vim.fn.fnameescape(h_path))
    if cpp_path then
      vim.cmd("vsplit " .. vim.fn.fnameescape(cpp_path))
    end
    vim.notify("UCore new: created " .. class_name, vim.log.levels.INFO)
  end)
end

local function detect_uvcs_provider(path)
  local ok_uvcs, uvcs = pcall(require, "uvcs")
  if not ok_uvcs or type(uvcs) ~= "table" or type(uvcs.detect_for_path) ~= "function" then
    return nil
  end

  local ok_provider, provider = pcall(uvcs.detect_for_path, path)
  if not ok_provider or type(provider) ~= "table" or type(provider.add_file) ~= "function" then
    return nil
  end

  return provider
end

function M.create(class_name)
  if not class_name or class_name == "" then
    vim.notify("UCore new: class name is required", vim.log.levels.ERROR)
    return
  end

  -- Remove extension if given
  class_name = class_name:gsub("%.h$", ""):gsub("%.cpp$", "")

  -- Validate UE naming
  if not class_name:match("^[UAFI]") then
    vim.notify("UCore new: class name should start with U/A/F/I", vim.log.levels.WARN)
    -- but still proceed
  end

  local root = project.find_project_root()
  if not root then
    vim.notify("UCore new: could not find .uproject", vim.log.levels.ERROR)
    return
  end

  -- Check for duplicates
  if check_name_exists(root, class_name) then
    vim.notify("UCore new: " .. class_name .. ".h already exists in project", vim.log.levels.ERROR)
    return
  end

  -- Pick source sub-directory: Root / Public / Private
  local default_dir = DIRECTORY_CHOICES[1]
  vim.ui.select(DIRECTORY_CHOICES, {
    prompt = "Create in which directory?",
    format_item = function(item)
      return item.label .. "  " .. item.desc
    end,
  }, function(choice)
    if not choice then return end
    local choice_key = choice.label:lower()

    -- Compute paths
    local buf_path = vim.api.nvim_buf_get_name(0)
    local current_dir = vim.fn.fnamemodify(buf_path ~= "" and buf_path or root, ":p:h")
    local module_dir = detect_module_dir(root, buf_path ~= "" and buf_path or root)
    local module_api = detect_module_api(root, buf_path ~= "" and buf_path or root)

    -- Relative path from module root to current directory
    local rel_path = ""
    if module_dir and current_dir:find(module_dir, 1, true) == 1 then
      rel_path = current_dir:sub(#module_dir + 2)
    end

    local h_dir, cpp_dir
    local choice_key = choice.label:lower()
    if choice_key == "root" then
      h_dir = current_dir
      cpp_dir = current_dir
    elseif choice_key == "public" then
      h_dir = module_dir .. "/Public"
      cpp_dir = module_dir .. "/Private"
      if rel_path ~= "" then
        h_dir = h_dir .. "/" .. rel_path
        cpp_dir = cpp_dir .. "/" .. rel_path
      end
    else
      h_dir = module_dir .. "/Private"
      cpp_dir = module_dir .. "/Private"
      if rel_path ~= "" then
        h_dir = h_dir .. "/" .. rel_path
        cpp_dir = cpp_dir .. "/" .. rel_path
      end
    end

    vim.fn.mkdir(h_dir, "p")
    vim.fn.mkdir(cpp_dir, "p")

    -- Pick parent class
    local default_parent = prefix_default_parent(class_name)
    ensure_parent_classes(root, function(items)
      -- Pre-select by prefix
      local pre_index = nil
      for i, name in ipairs(items) do
        if name == default_parent then
          pre_index = i
          break
        end
      end

      vim.ui.select(items, {
        prompt = "Select parent class (" .. #items .. " available):",
        format_item = function(item)
          return item
        end,
      }, function(parent_selection)
      if not parent_selection then return end
      local parent_class = parent_selection
      local parent_include = parent_header_path(parent_class, root, module_dir)
      local h_include_path = class_name .. ".h"

      -- Build .h content
      local h_content = build_h_template(class_name, parent_class, module_api, parent_include, choice_key)
      local h_path = h_dir .. "/" .. class_name .. ".h"

      -- Write .h
      local h_fd = io.open(h_path, "w")
      if not h_fd then
        vim.notify("UCore new: failed to write " .. h_path, vim.log.levels.ERROR)
        return
      end
      h_fd:write(h_content)
      h_fd:close()

      -- Build and write .cpp
      local module_name = vim.fn.fnamemodify(module_dir, ":t")
      local include_prefix = module_name
      if choice_key ~= "root" then
        include_prefix = module_name .. "/" .. choice_key
      end
      local inc_rel = rel_path ~= "" and rel_path .. "/" or ""
      local cpp_include = include_prefix .. "/" .. inc_rel .. class_name .. ".h"

      if class_name:match("^U") or class_name:match("^A") or not class_name:match("^F") then
        local cpp_content = build_cpp_template(class_name, cpp_include)
        local cpp_path = cpp_dir .. "/" .. class_name .. ".cpp"
        local cpp_fd = io.open(cpp_path, "w")
        if cpp_fd then
          cpp_fd:write(cpp_content)
          cpp_fd:close()
        end

        local provider = detect_uvcs_provider(h_path) or detect_uvcs_provider(cpp_path)
        if provider then
          local provider_name = type(provider.name) == "function" and provider.name() or "uvcs"
          vim.ui.select({ "Yes", "No" }, {
            prompt = string.format("%s add %s.h/.cpp?", tostring(provider_name), class_name),
          }, function(add_choice)
            if add_choice == "Yes" then
              local ok_h, err_h = provider.add_file(h_path, root)
              local ok_cpp, err_cpp = provider.add_file(cpp_path, root)
              if not ok_h then
                vim.notify("UCore new: add failed for " .. h_path .. "\n" .. tostring(err_h), vim.log.levels.ERROR)
              end
              if not ok_cpp then
                vim.notify("UCore new: add failed for " .. cpp_path .. "\n" .. tostring(err_cpp), vim.log.levels.ERROR)
              end
            end
            open_files(h_path, cpp_path, class_name)
          end)
        else
          open_files(h_path, cpp_path, class_name)
        end
      else
        open_files(h_path, nil, class_name)
      end
    end)
  end)
end)
end

return M
