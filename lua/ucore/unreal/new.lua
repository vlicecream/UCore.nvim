local project = require("ucore.project")
local remote = require("ucore.remote")
local write_access = require("ucore.write_access")
local select_ui = require("ucore.ui.select")

local M = {}

local FALLBACK_PARENT_CLASSES = {
  "UObject", "AActor", "APawn", "ACharacter",
  "UActorComponent", "USceneComponent", "UUserWidget", "UWidget",
  "UGameInstance", "UGameModeBase", "APlayerController", "APlayerState",
  "AGameStateBase", "USaveGame", "UDataAsset", "UPrimaryDataAsset",
  "UDeveloperSettings", "UBlueprintFunctionLibrary", "UAnimInstance",
  "UAnimMontage", "UAudioComponent",
}

local PARENT_CLASS_CACHE = nil
local SEARCH_PARENT_SENTINEL = "__ucore_search_parent__"

local DIRECTORY_CHOICES = {
  { label = "Root",  desc = "<Module>/" },
  { label = "Public", desc = "<Module>/Public/" },
  { label = "Private", desc = "<Module>/Private/" },
}

local function normalize_symbol_type(symbol_type)
  symbol_type = tostring(symbol_type or ""):lower()
  if symbol_type == "uclass" or symbol_type == "uinterface" then
    return "class"
  end
  if symbol_type == "ustruct" then
    return "struct"
  end
  return symbol_type
end

local function normalize_source(source)
  return tostring(source or ""):lower()
end

local function normalize_path(path)
  return tostring(path or ""):gsub("\\", "/")
end

local function strip_class_prefix(name)
  name = tostring(name or "")
  local prefix = name:sub(1, 1)
  if prefix == "U" or prefix == "A" or prefix == "F" or prefix == "I" then
    return name:sub(2)
  end
  return name
end

local function make_parent_item(name, opts)
  opts = opts or {}
  return {
    name = tostring(name or ""),
    type = tostring(opts.type or ""),
    path = normalize_path(opts.path or ""),
    module_name = tostring(opts.module_name or ""),
    source = tostring(opts.source or ""),
    display_path = tostring(opts.display_path or ""),
    is_fallback = opts.is_fallback == true,
  }
end

local function parent_item_key(item)
  local name = tostring(item and item.name or ""):lower()
  local path = normalize_path(item and item.path or ""):lower()
  local module_name = tostring(item and item.module_name or ""):lower()
  return table.concat({ name, path, module_name }, "\t")
end

local function parent_item_text(item)
  local parts = {
    tostring(item and item.name or ""),
    tostring(item and item.module_name or ""),
    normalize_path(item and item.path or ""),
    normalize_source(item and item.source or ""),
  }
  return table.concat(parts, " "):lower()
end

local function query_tokens(pattern)
  local tokens = {}
  for token in tostring(pattern or ""):lower():gmatch("%S+") do
    table.insert(tokens, token)
  end
  return tokens
end

local function matches_all_tokens(text, tokens)
  if #tokens == 0 then
    return true
  end

  for _, token in ipairs(tokens) do
    if not text:find(token, 1, true) then
      return false
    end
  end

  return true
end

local function parent_item_score(item, pattern)
  local query = vim.trim(tostring(pattern or "")):lower()
  local name = tostring(item.name or "")
  local lowered_name = name:lower()
  local path = normalize_path(item.path or ""):lower()
  local module_name = tostring(item.module_name or ""):lower()
  local source = normalize_source(item.source)
  local symbol_type = normalize_symbol_type(item.type)
  local stripped_name = strip_class_prefix(name):lower()

  local score = 0

  if query ~= "" then
    if lowered_name == query then
      score = score - 1200
    elseif stripped_name == query then
      score = score - 1150
    elseif lowered_name:find(query, 1, true) == 1 then
      score = score - 900
    elseif stripped_name:find(query, 1, true) == 1 then
      score = score - 820
    elseif module_name:find(query, 1, true) then
      score = score - 520
    elseif path:find(query, 1, true) then
      score = score - 360
    end
  end

  if symbol_type == "class" then
    score = score - 220
  elseif symbol_type == "struct" then
    score = score - 80
  end

  if name:match("^[UAI]") then
    score = score - 120
  elseif name:match("^F") then
    score = score + 30
  end

  if source == "project" then
    score = score - 40
  elseif source == "engine" then
    score = score + 10
  end

  if path:find("/private/", 1, true) then
    score = score + 70
  end
  if path:find("/editor/", 1, true) then
    score = score + 60
  end

  return score
end

local function sort_parent_items(items, pattern)
  table.sort(items, function(left, right)
    local left_score = parent_item_score(left, pattern)
    local right_score = parent_item_score(right, pattern)
    if left_score ~= right_score then
      return left_score < right_score
    end

    local left_name = tostring(left.name or "")
    local right_name = tostring(right.name or "")
    if left_name ~= right_name then
      return left_name < right_name
    end

    return tostring(left.path or "") < tostring(right.path or "")
  end)
end

local function format_parent_item(item)
  if type(item) ~= "table" then
    return tostring(item)
  end

  local name = tostring(item.name or "")
  local module_name = tostring(item.module_name or "")
  local source = normalize_source(item.source)
  local path = normalize_path(item.path or "")

  if source == "" and module_name == "" and path == "" then
    return name
  end

  local suffix = {}
  if source ~= "" then
    table.insert(suffix, source)
  end
  if module_name ~= "" then
    table.insert(suffix, module_name)
  end
  if path ~= "" then
    table.insert(suffix, path)
  end

  return string.format("%s  [%s]", name, table.concat(suffix, " | "))
end

local function ensure_parent_classes(root, callback)
  if PARENT_CLASS_CACHE then
    callback(PARENT_CLASS_CACHE)
    return
  end

  local items = {}
  local seen = {}
  for _, name in ipairs(FALLBACK_PARENT_CLASSES) do
    if not seen[name] then
      seen[name] = true
      table.insert(items, make_parent_item(name, { is_fallback = true }))
    end
  end
  PARENT_CLASS_CACHE = items
  callback(items)
end

local VALID_PARENT_SYMBOL_TYPES = {
  class = true,
  struct = true,
  uclass = true,
  ustruct = true,
  uinterface = true,
}

local function search_parent_classes(root, pattern, callback)
  pattern = vim.trim(pattern or "")
  if pattern == "" then
    callback({})
    return
  end

  remote.search_class_symbols(root, pattern, function(results, err)
    local merged = {}
    local seen = {}
    local tokens = query_tokens(pattern)

    local function push(item)
      if type(item) ~= "table" or item.name == "" then
        return
      end
      local key = parent_item_key(item)
      if seen[key] then
        return
      end
      if not matches_all_tokens(parent_item_text(item), tokens) then
        return
      end
      seen[key] = true
      table.insert(merged, item)
    end

    if not err and type(results) == "table" then
      for _, r in ipairs(results) do
        local name = tostring(r.name or r.text or "")
        local symbol_type = normalize_symbol_type(r.type)
        if VALID_PARENT_SYMBOL_TYPES[symbol_type] and name ~= "" then
          push(make_parent_item(name, {
            type = symbol_type,
            path = r.path,
            module_name = r.module_name,
            source = r.source,
          }))
        end
      end
    end

    ensure_parent_classes(root, function(common_items)
      for _, item in ipairs(common_items or {}) do
        push(item)
      end

      sort_parent_items(merged, pattern)
      callback(merged)
    end)
  end, 200)
end

local function has_module(name)
  local ok = pcall(require, name)
  return ok
end

local function pick_parent_class_live(root, common_items, default_parent, callback)
  local pickers = require("telescope.pickers")
  local finders = require("telescope.finders")
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")
  local sorters = require("telescope.sorters")

  local state = {
    query = "",
    common_items = common_items or {},
    remote_items = {},
    request_id = 0,
    input_seq = 0,
  }

  local picker_ref

  local function combined_items()
    local items = {}

    if vim.trim(state.query or "") == "" then
      for _, item in ipairs(state.common_items or {}) do
        table.insert(items, item)
      end
      return items
    end

    for _, item in ipairs(state.remote_items or {}) do
      table.insert(items, item)
    end

    return items
  end

  local function make_finder()
    return finders.new_table({
      results = combined_items(),
      entry_maker = function(item)
        return {
          value = item,
          display = format_parent_item(item),
          ordinal = parent_item_text(item),
        }
      end,
    })
  end

  local function refresh_picker()
    if picker_ref then
      pcall(function()
        picker_ref:refresh(make_finder(), { reset_prompt = false })
      end)
    end
  end

  local function request_remote(query)
    query = vim.trim(query or "")
    state.request_id = state.request_id + 1
    local request_id = state.request_id

    if query == "" then
      state.remote_items = {}
      refresh_picker()
      return
    end

    search_parent_classes(root, query, function(items)
      vim.schedule(function()
        if request_id ~= state.request_id then
          return
        end
        state.remote_items = items or {}
        refresh_picker()
      end)
    end)
  end

  picker_ref = pickers.new({}, {
    prompt_title = "Search parent class",
    default_text = nil,
    sorting_strategy = "ascending",
    selection_strategy = "row",
    finder = make_finder(),
    sorter = sorters.get_generic_fuzzy_sorter(),
    on_input_filter_cb = function(prompt)
      prompt = tostring(prompt or "")
      if prompt == state.query then
        return
      end

      state.query = prompt
      state.input_seq = state.input_seq + 1
      local input_seq = state.input_seq

      vim.defer_fn(function()
        if input_seq == state.input_seq then
          request_remote(state.query)
          refresh_picker()
        end
      end, 150)
    end,
    attach_mappings = function(prompt_bufnr, map)
      actions.select_default:replace(function()
        local selection = action_state.get_selected_entry()
        actions.close(prompt_bufnr)
        callback(selection and selection.value or nil)
      end)

      map("i", "<Esc>", function()
        actions.close(prompt_bufnr)
        callback(nil)
      end)
      return true
    end,
  })

  picker_ref:find()
end

local function choose_parent_class(root, default_parent, callback)
  ensure_parent_classes(root, function(common_items)
    local items = vim.list_extend({ SEARCH_PARENT_SENTINEL }, vim.deepcopy(common_items))
    vim.ui.select(items, {
      prompt = "Select parent class:",
      format_item = function(item)
        if item == SEARCH_PARENT_SENTINEL then
          return "Search..."
        end
        local name = type(item) == "table" and tostring(item.name or "") or tostring(item)
        local formatted = format_parent_item(item)
        if name == default_parent then
          return formatted .. "  (default)"
        end
        return formatted
      end,
    }, function(selection)
      if not selection then
        callback(nil)
        return
      end

      if selection ~= SEARCH_PARENT_SENTINEL then
        callback(selection)
        return
      end

      if has_module("telescope.pickers") then
        return pick_parent_class_live(root, common_items, default_parent, callback)
      end

      select_ui.input({
        title = "Search parent class",
        default = "",
      }, function(input)
        input = vim.trim(input or "")
        if input == "" then
          callback(nil)
          return
        end

        search_parent_classes(root, input, function(search_items)
          if #search_items == 0 then
            vim.notify("UCore new: no parent classes found for " .. input, vim.log.levels.WARN)
            callback(nil)
            return
          end

          vim.ui.select(search_items, {
            prompt = "Search results:",
            format_item = function(item)
              return format_parent_item(item)
            end,
          }, callback)
        end)
      end)
    end)
  end)
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

local function file_exists(path)
  return vim.fn.filereadable(path) == 1
end

local function prefix_default_parent(class_name)
  if class_name:match("^A") then return "AActor" end
  if class_name:match("^U") then return "UObject" end
  if class_name:match("^F") then return nil end
  if class_name:match("^I") then return nil end
  return "UObject"
end

local function normalize(path)
  return (path or ""):gsub("\\", "/")
end

local function trim_module_include(path)
  path = normalize(path)
  return path:match("/Public/(.+)$")
    or path:match("/Private/(.+)$")
    or path:match("/Source/[^/]+/(.+)$")
    or path:match("/Classes/(.+)$")
    or path
end

local ENGINE_PARENT_HEADERS = {
  UObject = "UObject/Object.h",
  UInterface = "UObject/Interface.h",
  AActor = "GameFramework/Actor.h",
  APawn = "GameFramework/Pawn.h",
  ACharacter = "GameFramework/Character.h",
  UActorComponent = "Components/ActorComponent.h",
  USceneComponent = "Components/SceneComponent.h",
  UUserWidget = "Blueprint/UserWidget.h",
  UWidget = "Components/Widget.h",
  UGameInstance = "Engine/GameInstance.h",
  UGameModeBase = "GameFramework/GameModeBase.h",
  APlayerController = "GameFramework/PlayerController.h",
  APlayerState = "GameFramework/PlayerState.h",
  AGameStateBase = "GameFramework/GameStateBase.h",
  USaveGame = "GameFramework/SaveGame.h",
  UDataAsset = "Engine/DataAsset.h",
  UPrimaryDataAsset = "Engine/DataAsset.h",
  UDeveloperSettings = "Engine/DeveloperSettings.h",
  UBlueprintFunctionLibrary = "Kismet/BlueprintFunctionLibrary.h",
  UAnimInstance = "Animation/AnimInstance.h",
  UAudioComponent = "Components/AudioComponent.h",
}

local function parent_header_path(parent_class, root, module_dir, parent_item)
  if not parent_class or parent_class == "" then
    return nil
  end

  if ENGINE_PARENT_HEADERS[parent_class] then
    return ENGINE_PARENT_HEADERS[parent_class]
  end

  if type(parent_item) == "table" and tostring(parent_item.path or "") ~= "" then
    return trim_module_include(parent_item.path)
  end

  local search_roots = { normalize(root) }
  local engine = project.cached_engine_metadata(root) or project.engine_metadata(root)
  local engine_root = type(engine) == "table" and normalize(engine.engine_root or "") or ""
  if engine_root ~= "" then
    table.insert(search_roots, engine_root)
  end

  local basename_candidates = {
    parent_class,
    strip_class_prefix(parent_class),
  }

  local files = {}
  local seen = {}
  for _, search_root in ipairs(search_roots) do
    if search_root ~= "" then
      for _, basename in ipairs(basename_candidates) do
        basename = vim.trim(tostring(basename or ""))
        if basename ~= "" then
          local pattern = search_root .. "/**/" .. basename .. ".h"
          for _, path in ipairs(vim.fn.glob(pattern, false, true)) do
            path = normalize(path)
            if path ~= "" and not seen[path] then
              seen[path] = true
              table.insert(files, path)
            end
          end
        end
      end
    end
  end

  if #files == 0 then
    local short_name = strip_class_prefix(parent_class)
    return (short_name ~= "" and short_name or parent_class) .. ".h"
  end

  table.sort(files, function(a, b)
    a = normalize(a)
    b = normalize(b)

    local a_is_module = module_dir and a:find(normalize(module_dir), 1, true) == 1
    local b_is_module = module_dir and b:find(normalize(module_dir), 1, true) == 1
    if a_is_module ~= b_is_module then
      return a_is_module
    end

    local a_public = a:find("/Public/", 1, true) ~= nil
    local b_public = b:find("/Public/", 1, true) ~= nil
    if a_public ~= b_public then
      return a_public
    end

    return #a < #b
  end)

  return trim_module_include(files[1])
end

local function header_include_path(header_path, module_dir)
  header_path = normalize(header_path)
  module_dir = normalize(module_dir)

  local trimmed = trim_module_include(header_path)
  if trimmed and trimmed ~= "" and trimmed ~= header_path then
    return trimmed
  end

  if module_dir ~= "" and header_path:find(module_dir, 1, true) == 1 then
    local rel = header_path:sub(#module_dir + 2)
    rel = rel:gsub("^Public/", ""):gsub("^Private/", "")
    return rel
  end

  return vim.fn.fnamemodify(header_path, ":t")
end

local function build_h_template(class_name, parent_class, module_api, parent_include, _choice_key)
  local lines = {
    "#pragma once",
    "",
    '#include "CoreMinimal.h"',
  }

  if parent_include and parent_include ~= "" then
    table.insert(lines, '#include "' .. parent_include .. '"')
  end

  local prefix = class_name:sub(1, 1)

  if prefix == "U" or prefix == "A" then
    table.insert(lines, '#include "' .. class_name .. '.generated.h"')
    table.insert(lines, "")
    table.insert(lines, "UCLASS()")
    table.insert(lines, "class " .. module_api .. " " .. class_name .. " : public " .. parent_class)
    table.insert(lines, "{")
    table.insert(lines, "\tGENERATED_BODY()")
    table.insert(lines, "")
    table.insert(lines, "public:")
    table.insert(lines, "\t" .. class_name .. "();")
    table.insert(lines, "};")
    return table.concat(lines, "\n") .. "\n"
  end

  if prefix == "F" then
    table.insert(lines, "")
    table.insert(lines, "struct " .. module_api .. " " .. class_name)
    table.insert(lines, "{")
    table.insert(lines, "};")
    return table.concat(lines, "\n") .. "\n"
  end

  table.insert(lines, "")
  table.insert(lines, "class " .. module_api .. " " .. class_name)
  if parent_class and parent_class ~= "" then
    table.insert(lines, " : public " .. parent_class)
  end
  table.insert(lines, "")
  table.insert(lines, "{")
  table.insert(lines, "public:")
  table.insert(lines, "\tvirtual ~" .. class_name .. "() = default;")
  table.insert(lines, "};")
  return table.concat(lines, "\n") .. "\n"
end

local function build_cpp_template(class_name, cpp_include)
  local lines = {
    '#include "' .. cpp_include .. '"',
    "",
  }

  if class_name:match("^[UA]") then
    table.insert(lines, class_name .. "::" .. class_name .. "()")
    table.insert(lines, "{")
    table.insert(lines, "}")
    table.insert(lines, "")
  end

  return table.concat(lines, "\n")
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
  local provider = write_access.detect_provider(path)
  if type(provider) ~= "table" or type(provider.add_file) ~= "function" then
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

  -- Pick source sub-directory: Root / Public / Private
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

    local h_path = h_dir .. "/" .. class_name .. ".h"
    local cpp_path = cpp_dir .. "/" .. class_name .. ".cpp"

    if file_exists(h_path) then
      vim.notify("UCore new: " .. class_name .. ".h already exists in target directory", vim.log.levels.ERROR)
      return
    end

    if class_name:match("^[UA]") and file_exists(cpp_path) then
      vim.notify("UCore new: " .. class_name .. ".cpp already exists in target directory", vim.log.levels.ERROR)
      return
    end

    vim.fn.mkdir(h_dir, "p")
    vim.fn.mkdir(cpp_dir, "p")

    -- Pick parent class
    local default_parent = prefix_default_parent(class_name)
    choose_parent_class(root, default_parent, function(parent_selection)
        if not parent_selection then return end
        local parent_item = type(parent_selection) == "table"
          and parent_selection
          or make_parent_item(parent_selection)
        local parent_class = tostring(parent_item.name or "")
        local parent_include = parent_header_path(parent_class, root, module_dir, parent_item)

        local h_content = build_h_template(class_name, parent_class, module_api, parent_include, choice_key)
        local h_fd = io.open(h_path, "w")
        if not h_fd then
          vim.notify("UCore new: failed to write " .. h_path, vim.log.levels.ERROR)
          return
        end
        h_fd:write(h_content)
        h_fd:close()

        local should_create_cpp = class_name:match("^[UA]") ~= nil
        if should_create_cpp then
          local cpp_include = header_include_path(h_path, module_dir)
          local cpp_content = build_cpp_template(class_name, cpp_include)
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
end

return M
