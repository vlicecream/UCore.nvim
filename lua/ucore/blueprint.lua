local config = require("ucore.config")
local project = require("ucore.project")
local remote = require("ucore.remote")
local ui = require("ucore.ui")

local M = {}
local ns = vim.api.nvim_create_namespace("ucore_blueprint")
local group_name = "UCoreBlueprint"
local pending = {}
local refresh_seq = {}

local function normalize_path(path)
	return tostring(path or ""):gsub("\\", "/")
end

local function normalize_lower(path)
	return normalize_path(path):lower()
end

local function current_content(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

local function current_context()
	local bufnr = vim.api.nvim_get_current_buf()
	local file_path = vim.api.nvim_buf_get_name(bufnr)
	if file_path == "" then
		return nil, "Current buffer has no file path"
	end

	local root = project.find_project_root(file_path)
	if not root then
		return nil, "Current buffer is not inside an Unreal project"
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	return {
		bufnr = bufnr,
		root = root,
		file_path = normalize_path(file_path),
		content = current_content(bufnr),
		line = cursor[1] - 1,
		character = cursor[2],
		cword = tostring(vim.fn.expand("<cword>") or ""),
	}, nil
end

local function list_value(value)
	return type(value) == "table" and value or {}
end

local function text_value(value)
	if value == nil or value == vim.NIL then
		return ""
	end
	return tostring(value)
end

local function trim_unreal_suffix(name)
	name = tostring(name or "")
	name = name:gsub("_Implementation$", "")
	name = name:gsub("_Validate$", "")
	return name
end

local function blueprint_config()
	local value = config.values.blueprint
	return type(value) == "table" and value or {}
end

local function is_enabled()
	return blueprint_config().enable ~= false
end

local function debounce_ms()
	return tonumber(blueprint_config().debounce_ms or 300) or 300
end

local function is_function_cursor(cursor_info)
	local parameters = text_value(cursor_info.parameters)
	return parameters ~= ""
end

local function relation_label(target_kind, category)
	if target_kind == "class" then
		if category == "derived" then
			return "Derived Blueprint"
		end
		return "Blueprint Reference"
	end

	if category == "derived" then
		return "Derived Blueprint"
	end

	return "Blueprint Call/Override"
end

local function asset_item(asset_path, category, target)
	local path = text_value(asset_path)
	return {
		name = vim.fn.fnamemodify(path, ":t"),
		type = "asset",
		symbol_type = "uasset",
		source = relation_label(target.kind, category),
		path = path,
		asset_path = path,
		blueprint_category = category,
		target_name = target.name,
		target_kind = target.kind,
	}
end

local function push_unique_asset(items, seen, asset_path, category, target)
	asset_path = text_value(asset_path)
	if asset_path == "" then
		return
	end

	local key = category .. "::" .. asset_path:lower()
	if seen[key] then
		return
	end

	seen[key] = true
	table.insert(items, asset_item(asset_path, category, target))
end

local function class_hint_text(derived_count, reference_count)
	local chunks = {}
	if derived_count > 0 then
		table.insert(chunks, "Derived " .. derived_count)
	end
	if reference_count > 0 then
		table.insert(chunks, "Refs " .. reference_count)
	end
	if vim.tbl_isempty(chunks) then
		return nil
	end
	return "Blueprint " .. table.concat(chunks, "  ")
end

local function member_hint_text(reference_count)
	if reference_count <= 0 then
		return nil
	end
	return "Blueprint " .. reference_count
end

local function member_is_blueprint_candidate(member)
	local flags = text_value(member.flags):upper()
	return flags:find("UFUNCTION", 1, true) ~= nil or flags:find("UPROPERTY", 1, true) ~= nil
end

local function collect_file_targets(symbols, file_path)
	local current_path = normalize_lower(file_path)
	local items = {}
	local seen = {}

	for _, symbol in ipairs(list_value(symbols)) do
		local symbol_name = text_value(symbol.name)
		local symbol_line = tonumber(symbol.line or 0) or 0
		local symbol_kind = text_value(symbol.kind):lower()
		if symbol_name ~= "" and symbol_line > 0 and (symbol_kind == "class" or symbol_kind == "struct") then
			local key = string.format("class:%s:%d", symbol_name, symbol_line)
			if not seen[key] then
				seen[key] = true
				table.insert(items, {
					kind = "class",
					name = symbol_name,
					line = symbol_line,
				})
			end
		end

		for _, member in ipairs(list_value(symbol.members)) do
			local member_path = normalize_lower(member.file_path or symbol.file_path or file_path)
			if member_path == "" or member_path == current_path then
				if member_is_blueprint_candidate(member) then
					local member_name = trim_unreal_suffix(text_value(member.name))
					local member_line = tonumber(member.line or 0) or 0
					if member_name ~= "" and member_line > 0 then
						local member_kind = text_value(member.return_type) ~= "" and "function" or "member"
						local key = string.format("%s:%s:%d", member_kind, member_name, member_line)
						if not seen[key] then
							seen[key] = true
							table.insert(items, {
								kind = member_kind,
								name = member_name,
								line = member_line,
							})
						end
					end
				end
			end
		end
	end

	return items
end

local function clear(bufnr)
	if vim.api.nvim_buf_is_valid(bufnr) then
		vim.api.nvim_buf_clear_namespace(bufnr, ns, 0, -1)
	end
end

local function apply_hints(bufnr, items)
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	clear(bufnr)

	for _, item in ipairs(items or {}) do
		local text = item.hint_text
		local line = tonumber(item.line or 0) or 0
		if text and text ~= "" and line > 0 then
			local row = line - 1
			if item.kind == "class" then
				vim.api.nvim_buf_set_extmark(bufnr, ns, row, 0, {
					virt_lines = { { { text, "Comment" } } },
					virt_lines_above = true,
					priority = 90,
				})
			else
				vim.api.nvim_buf_set_extmark(bufnr, ns, row, 0, {
					virt_text = { { "  " .. text, "Comment" } },
					virt_text_pos = "eol",
					priority = 90,
				})
			end
		end
	end
end

local function exact_class_match(items, name)
	name = text_value(name)
	for _, item in ipairs(list_value(items)) do
		if text_value(item.name) == name then
			return item
		end
	end
	return nil
end

local function resolve_target_from_parse(ctx, parse_result)
	local cursor_info = type(parse_result) == "table" and parse_result.cursor_info or {}
	local name = text_value(cursor_info.name)
	local cword = ctx.cword

	if name ~= "" then
		local base_name = trim_unreal_suffix(name)
		if cword == "" or cword == name or cword == base_name then
			return {
				kind = is_function_cursor(cursor_info) and "function" or "member",
				name = base_name ~= "" and base_name or name,
				class_name = text_value(cursor_info.class_name),
				cursor_info = cursor_info,
			}
		end
	end

	return nil
end

local function resolve_target(ctx, callback)
	remote.parse_buffer(ctx.root, {
		content = ctx.content,
		file_path = ctx.file_path,
		line = ctx.line,
		character = ctx.character,
	}, function(parse_result, parse_err)
		local parsed_target = not parse_err and resolve_target_from_parse(ctx, parse_result) or nil
		local cword = ctx.cword
		if cword == "" then
			return callback(parsed_target, parse_err)
		end

		remote.search_class_symbols(ctx.root, cword, function(search_result, _)
			local class_item = exact_class_match(search_result, cword)
			if class_item then
				return callback({
					kind = "class",
					name = text_value(class_item.name),
					path = text_value(class_item.path),
					class_item = class_item,
				}, nil)
			end

			callback(parsed_target, parse_err)
		end, { limit = 20, offset = 0 })
	end)
end

local function show_target_picker(target, items)
	if vim.tbl_isempty(items) then
		vim.notify("UCore blueprint: no related Blueprint assets found for " .. target.name, vim.log.levels.INFO)
		return
	end

	ui.select.find(items, {
		default_text = target.name,
	})
end

local function collect_function_or_member_assets(ctx, target)
	remote.get_asset_usages(ctx.root, target.name, function(result, err)
		if err then
			return vim.notify("UCore blueprint failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		local items = {}
		local seen = {}
		for _, asset_path in ipairs(list_value(result and result.references)) do
			push_unique_asset(items, seen, asset_path, "references", target)
		end
		show_target_picker(target, items)
	end)
end

local function collect_class_assets(ctx, target)
	local pending_count = 2
	local references = nil
	local derived = nil
	local first_err = nil

	local function finish()
		pending_count = pending_count - 1
		if pending_count > 0 then
			return
		end

		if first_err and references == nil and derived == nil then
			return vim.notify("UCore blueprint failed:\n" .. tostring(first_err), vim.log.levels.ERROR)
		end

		local items = {}
		local seen = {}

		for _, item in ipairs(list_value(derived)) do
			push_unique_asset(items, seen, item.asset_path or item.path or item.name, "derived", target)
		end

		for _, asset_path in ipairs(list_value(references and references.references)) do
			push_unique_asset(items, seen, asset_path, "references", target)
		end

		for _, asset_path in ipairs(list_value(references and references.derived)) do
			push_unique_asset(items, seen, asset_path, "derived", target)
		end

		show_target_picker(target, items)
	end

	remote.find_derived_classes(ctx.root, target.name, function(result, err)
		if err and not first_err then
			first_err = err
		end
		derived = result
		finish()
	end)

	remote.get_asset_usages(ctx.root, target.name, function(result, err)
		if err and not first_err then
			first_err = err
		end
		references = result
		finish()
	end)
end

local function fetch_class_hint(root, target, callback)
	local pending_count = 2
	local derived_result = nil
	local usage_result = nil

	local function finish()
		pending_count = pending_count - 1
		if pending_count > 0 then
			return
		end

		local derived_count = #list_value(derived_result)
		if type(usage_result) == "table" then
			local extra_derived = #list_value(usage_result.derived)
			if extra_derived > derived_count then
				derived_count = extra_derived
			end
		end
		local reference_count = type(usage_result) == "table" and #list_value(usage_result.references) or 0

		callback(vim.tbl_extend("force", target, {
			derived_count = derived_count,
			reference_count = reference_count,
			hint_text = class_hint_text(derived_count, reference_count),
		}))
	end

	remote.find_derived_classes(root, target.name, function(result)
		derived_result = result
		finish()
	end)

	remote.get_asset_usages(root, target.name, function(result)
		usage_result = result
		finish()
	end)
end

local function fetch_member_hint(root, target, callback)
	remote.get_asset_usages(root, target.name, function(result)
		local reference_count = type(result) == "table" and #list_value(result.references) or 0
		callback(vim.tbl_extend("force", target, {
			reference_count = reference_count,
			hint_text = member_hint_text(reference_count),
		}))
	end)
end

local function refresh_buffer(bufnr)
	if not is_enabled() then
		clear(bufnr)
		return
	end

	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local file_path = vim.api.nvim_buf_get_name(bufnr)
	if file_path == "" then
		clear(bufnr)
		return
	end

	local root = project.find_project_root(file_path)
	if not root then
		clear(bufnr)
		return
	end

	local seq = (refresh_seq[bufnr] or 0) + 1
	refresh_seq[bufnr] = seq

	remote.get_file_symbols(root, file_path, function(symbols, err)
		if err or type(symbols) ~= "table" then
			if refresh_seq[bufnr] == seq then
				vim.schedule(function()
					clear(bufnr)
				end)
			end
			return
		end

		local targets = collect_file_targets(symbols, file_path)
		if vim.tbl_isempty(targets) then
			if refresh_seq[bufnr] == seq then
				vim.schedule(function()
					clear(bufnr)
				end)
			end
			return
		end

		local remaining = #targets
		local resolved = {}

		local function on_item_done(item)
			if refresh_seq[bufnr] ~= seq or not vim.api.nvim_buf_is_valid(bufnr) then
				return
			end

			table.insert(resolved, item)
			remaining = remaining - 1
			if remaining == 0 then
				vim.schedule(function()
					if refresh_seq[bufnr] == seq then
						apply_hints(bufnr, resolved)
					end
				end)
			end
		end

		for _, target in ipairs(targets) do
			if target.kind == "class" then
				fetch_class_hint(root, target, on_item_done)
			else
				fetch_member_hint(root, target, on_item_done)
			end
		end
	end)
end

function M.schedule(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if pending[bufnr] then
		pending[bufnr]:stop()
		pending[bufnr]:close()
	end

	local timer = vim.loop.new_timer()
	pending[bufnr] = timer

	timer:start(debounce_ms(), 0, function()
		pending[bufnr] = nil
		timer:close()
		vim.schedule(function()
			refresh_buffer(bufnr)
		end)
	end)
end

function M.show_related()
	local ctx, err = current_context()
	if not ctx then
		return vim.notify("UCore blueprint: " .. tostring(err), vim.log.levels.WARN)
	end

	resolve_target(ctx, function(target, resolve_err)
		if not target or text_value(target.name) == "" then
			local message = resolve_err and tostring(resolve_err) or "could not resolve class/function/property under cursor"
			return vim.notify("UCore blueprint: " .. message, vim.log.levels.WARN)
		end

		if target.kind == "class" then
			return collect_class_assets(ctx, target)
		end

		return collect_function_or_member_assets(ctx, target)
	end)
end

function M.setup()
	if not is_enabled() then
		return
	end

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	vim.api.nvim_create_autocmd({
		"BufReadPost",
		"BufEnter",
		"BufWritePost",
		"TextChanged",
		"InsertLeave",
	}, {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx" },
		callback = function(args)
			M.schedule(args.buf)
		end,
	})

	vim.api.nvim_create_autocmd("BufDelete", {
		group = group,
		callback = function(args)
			if pending[args.buf] then
				pending[args.buf]:stop()
				pending[args.buf]:close()
				pending[args.buf] = nil
			end
			refresh_seq[args.buf] = nil
		end,
	})
end

function M.reset()
	for bufnr, timer in pairs(pending) do
		if timer then
			timer:stop()
			timer:close()
		end
		pending[bufnr] = nil
	end

	refresh_seq = {}
	pcall(vim.api.nvim_del_augroup_by_name, group_name)

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		clear(bufnr)
	end
end

return M
