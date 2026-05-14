local config = require("ucore.config")

local M = {}

local function debug_enabled()
	local progress_config = config.values.progress or {}
	return progress_config.log == true
end

local function log_path()
	local data_dir = vim.fn.stdpath("data")
	vim.fn.mkdir(data_dir .. "/ucore", "p")
	return data_dir .. "/ucore/query-debug.log"
end

local function stringify(value)
	if type(value) == "table" then
		local ok, encoded = pcall(vim.json.encode, value)
		if ok then
			return encoded
		end
	end

	return tostring(value)
end

function M.log(...)
	if not debug_enabled() then
		return
	end

	local parts = {}
	for index = 1, select("#", ...) do
		table.insert(parts, stringify(select(index, ...)))
	end

	local line = string.format("[%s] %s\n", os.date("%H:%M:%S"), table.concat(parts, " "))
	local fd = io.open(log_path(), "a")
	if not fd then
		return
	end

	fd:write(line)
	fd:close()
end

function M.count_items(items)
	if type(items) ~= "table" then
		return 0
	end
	return #items
end

function M.summarize_query(query)
	if type(query) ~= "table" then
		return tostring(query)
	end

	local summary = {
		kind = query.kind,
		file_path = query.file_path,
		line = query.line,
		character = query.character,
		pattern = query.pattern,
		scope = query.scope,
		limit = query.limit,
		offset = query.offset,
		part = query.part,
		class_name = query.class_name,
		base_class = query.base_class,
		symbol_name = query.symbol_name,
		asset_path = query.asset_path,
		engine_db_path = query.engine_db_path,
		project_root = query.project_root,
		content_len = type(query.content) == "string" and #query.content or nil,
		open_files = type(query.open_files) == "table" and #query.open_files or nil,
		output_len = type(query.output) == "string" and #query.output or nil,
	}

	return summary
end

local function summarize_item(item)
	if type(item) ~= "table" then
		return tostring(item)
	end

	return {
		name = item.name or item.label or item.symbol_name,
		type = item.type or item.kind or item.symbol_type,
		class_name = item.class_name or item.owner_class or item.sourceClass,
		path = item.path or item.file_path or item.asset_path,
		line = item.line or item.line_number,
		source = item.source,
	}
end

function M.summarize_result(result)
	if result == nil then
		return "nil"
	end

	if vim.NIL and result == vim.NIL then
		return "vim.NIL"
	end

	if type(result) ~= "table" then
		return tostring(result)
	end

	local items = result.items or result.results or result.completions or result.signatures
	if type(items) == "table" then
		return {
			count = #items,
			sample = summarize_item(items[1]),
		}
	end

	if result.file_path or result.path or result.asset_path then
		return summarize_item(result)
	end

	return {
		keys = vim.tbl_keys(result),
	}
end

return M
