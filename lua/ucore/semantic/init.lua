local config = require("ucore.config")
local project = require("ucore.project")
local remote = require("ucore.remote")

local M = {}

local ns = vim.api.nvim_create_namespace("ucore_semantic")
local group_name = "UCoreSemantic"
local pending = {}

local function normalize_path(path)
	return tostring(path or ""):gsub("\\", "/"):lower()
end

local function is_identifier_byte(byte)
	return byte and (byte == 95 or byte >= 48 and byte <= 57 or byte >= 65 and byte <= 90 or byte >= 97 and byte <= 122)
end

local function find_identifier_col(line_text, name)
	line_text = tostring(line_text or "")
	name = tostring(name or "")

	if name == "" then
		return nil
	end

	local start = 1
	while true do
		local first, last = line_text:find(name, start, true)
		if not first then
			return nil
		end

		local before = first > 1 and line_text:byte(first - 1) or nil
		local after = last < #line_text and line_text:byte(last + 1) or nil

		if not is_identifier_byte(before) and not is_identifier_byte(after) then
			return first - 1
		end

		start = last + 1
	end
end

local function highlight_for_symbol_kind(kind)
	kind = tostring(kind or ""):lower()

	if kind:find("enum", 1, true) then
		return "@type.enum.unreal_cpp"
	end

	return "@type.unreal_cpp"
end

local function highlight_for_member(member)
	if member.return_type and member.return_type ~= vim.NIL and member.return_type ~= "" then
		return "@function.method.unreal_cpp"
	end

	return "@property.unreal_cpp"
end

local function mark_name(bufnr, name, line, hl_group)
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	line = tonumber(line or 0)
	if not line or line <= 0 then
		return
	end

	local row = line - 1
	if row >= vim.api.nvim_buf_line_count(bufnr) then
		return
	end

	local line_text = vim.api.nvim_buf_get_lines(bufnr, row, row + 1, false)[1] or ""
	local col = find_identifier_col(line_text, name)
	if not col then
		return
	end

	vim.api.nvim_buf_set_extmark(bufnr, ns, row, col, {
		end_col = col + #name,
		hl_group = hl_group,
		priority = 160,
	})
end

local function mark_node(bufnr, node, hl_group)
	if not vim.api.nvim_buf_is_valid(bufnr) or not node then
		return
	end

	local row, col, end_row, end_col = node:range()
	vim.api.nvim_buf_set_extmark(bufnr, ns, row, col, {
		end_row = end_row,
		end_col = end_col,
		hl_group = hl_group,
		priority = 160,
	})
end

local function node_text(bufnr, node)
	return vim.treesitter.get_node_text(node, bufnr)
end

local function child_by_field(node, field_name)
	if not node then
		return nil
	end

	if node.child_by_field_name then
		return node:child_by_field_name(field_name)
	end

	if node.field then
		local children = node:field(field_name)
		return children and children[1] or nil
	end

	return nil
end

local function iter_named(node, callback)
	if not node then
		return
	end

	callback(node)

	for index = 0, node:named_child_count() - 1 do
		iter_named(node:named_child(index), callback)
	end
end

local function rightmost_identifier(node)
	if not node then
		return nil
	end

	local kind = node:type()
	if kind == "identifier" or kind == "field_identifier" then
		return node
	end

	for index = node:named_child_count() - 1, 0, -1 do
		local found = rightmost_identifier(node:named_child(index))
		if found then
			return found
		end
	end

	return nil
end

local function declarator_identifier(node)
	if not node then
		return nil
	end

	local declarator = child_by_field(node, "declarator")
	return rightmost_identifier(declarator)
end

local function collect_local_names(bufnr, scope)
	local names = {}

	iter_named(scope.node, function(node)
		local kind = node:type()
		if kind == "parameter_declaration" or kind == "declaration" then
			local name_node = declarator_identifier(node)
			if name_node then
				names[node_text(bufnr, name_node)] = true
			end
		end
	end)

	return names
end

local function extract_qualified_scope(bufnr, function_node)
	local declarator_text = node_text(bufnr, child_by_field(function_node, "declarator") or function_node)
	local class_name = declarator_text:match("([A-Za-z_][%w_]*)::[~A-Za-z_][%w_]*%s*%(")

	if class_name then
		return class_name
	end

	return nil
end

local function collect_function_scopes(bufnr)
	local ok, parser = pcall(vim.treesitter.get_parser, bufnr)
	if not ok or not parser then
		return {}
	end

	local trees = parser:parse()
	local tree = trees and trees[1]
	if not tree then
		return {}
	end

	local scopes = {}
	local root = tree:root()

	iter_named(root, function(node)
		if node:type() ~= "function_definition" then
			return
		end

		local class_name = extract_qualified_scope(bufnr, node)
		if not class_name then
			return
		end

		table.insert(scopes, {
			node = node,
			class_name = class_name,
			local_names = collect_local_names(bufnr, { node = node }),
		})
	end)

	return scopes
end

local function class_names_from_scopes(scopes)
	local seen = {}
	local names = {}

	for _, scope in ipairs(scopes or {}) do
		if scope.class_name and not seen[scope.class_name] then
			seen[scope.class_name] = true
			table.insert(names, scope.class_name)
		end
	end

	return names
end

local function member_name_map(members)
	local map = {}

	for _, member in ipairs(members or {}) do
		local name = tostring(member.name or "")
		if name ~= "" then
			map[name] = highlight_for_member(member)
		end
	end

	return map
end

local function apply_member_usages(bufnr, scopes, class_members)
	for _, scope in ipairs(scopes or {}) do
		local members = class_members[scope.class_name] or {}
		if vim.tbl_isempty(members) then
			goto continue
		end

		iter_named(scope.node, function(node)
			local kind = node:type()
			if kind ~= "identifier" and kind ~= "field_identifier" then
				return
			end

			local name = node_text(bufnr, node)
			local hl_group = members[name]
			if not hl_group or scope.local_names[name] then
				return
			end

			mark_node(bufnr, node, hl_group)
		end)

		::continue::
	end
end

local function apply_symbols(bufnr, file_path, symbols, scopes, class_members)
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	vim.api.nvim_buf_clear_namespace(bufnr, ns, 0, -1)

	local current_path = normalize_path(file_path)

	for _, symbol in ipairs(symbols or {}) do
		mark_name(bufnr, symbol.name, symbol.line, highlight_for_symbol_kind(symbol.kind))

		for _, member in ipairs(symbol.members or {}) do
			local member_path = normalize_path(member.file_path or symbol.file_path or file_path)
			if member_path == "" or member_path == current_path then
				mark_name(bufnr, member.name, member.line, highlight_for_member(member))
			end
		end
	end

	apply_member_usages(bufnr, scopes or {}, class_members or {})
end

local function fetch_class_members(root, class_names, callback)
	if vim.tbl_isempty(class_names) then
		return callback({})
	end

	local remaining = #class_names
	local results = {}

	for _, class_name in ipairs(class_names) do
		remote.get_class_members(root, class_name, function(result, err)
			if not err and type(result) == "table" then
				results[class_name] = member_name_map(result)
			else
				results[class_name] = {}
			end

			remaining = remaining - 1
			if remaining == 0 then
				callback(results)
			end
		end)
	end
end

function M.refresh(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if not config.values.semantic or config.values.semantic.enable == false then
		return
	end

	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local file_path = vim.api.nvim_buf_get_name(bufnr)
	if file_path == "" then
		return
	end

	local root = project.find_project_root(file_path)
	if not root then
		vim.api.nvim_buf_clear_namespace(bufnr, ns, 0, -1)
		return
	end

	local scopes = collect_function_scopes(bufnr)
	local class_names = class_names_from_scopes(scopes)

	remote.get_file_symbols(root, file_path, function(result, err)
		if err or type(result) ~= "table" then
			return
		end

		fetch_class_members(root, class_names, function(class_members)
			vim.schedule(function()
				apply_symbols(bufnr, file_path, result, scopes, class_members)
			end)
		end)
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

	timer:start(config.values.semantic.debounce_ms or 120, 0, function()
		pending[bufnr] = nil
		timer:close()

		vim.schedule(function()
			M.refresh(bufnr)
		end)
	end)
end

function M.clear(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()

	if vim.api.nvim_buf_is_valid(bufnr) then
		vim.api.nvim_buf_clear_namespace(bufnr, ns, 0, -1)
	end
end

function M.setup()
	local group = vim.api.nvim_create_augroup(group_name, { clear = true })

	vim.api.nvim_create_autocmd({
		"BufReadPost",
		"BufEnter",
		"BufWritePost",
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
		end,
	})
end

return M
