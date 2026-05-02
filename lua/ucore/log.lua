local config = require("ucore.config")
local project = require("ucore.project")
local server = require("ucore.server")

local M = {}

local function ensure_parent_dir(path)
	local parent = vim.fn.fnamemodify(path, ":h")
	if parent and parent ~= "" then
		vim.fn.mkdir(parent, "p")
	end
end

local function fallback_log_path()
	local ok_root, root = pcall(project.find_project_root_from_context, {
		registered_fallback = true,
	})
	if ok_root and root then
		return project.build_paths(root).log_path
	end

	return config.values.cache_dir .. "/ucore.log"
end

local function active_log_path()
	local path = server.log_path()
	if path and path ~= "" then
		return path
	end

	return fallback_log_path()
end

local function compact_string(text, limit)
	text = tostring(text or "")
	text = text:gsub("[\r\n\t]", " ")

	if #text > limit then
		return text:sub(1, limit) .. "..."
	end

	return text
end

local function encode_value(value)
	if value == nil then
		return "nil"
	end

	local value_type = type(value)
	if value_type == "boolean" or value_type == "number" then
		return tostring(value)
	end

	if value_type == "string" then
		return '"' .. compact_string(value, 160) .. '"'
	end

	if value_type == "table" then
		local ok, encoded = pcall(vim.json.encode, value)
		if ok and encoded then
			return compact_string(encoded, 240)
		end

		return compact_string(vim.inspect(value), 240)
	end

	return '"' .. compact_string(tostring(value), 160) .. '"'
end

function M.write(tag, fields)
	local path = active_log_path()
	ensure_parent_dir(path)

	local line = {
		os.date("%Y-%m-%d %H:%M:%S"),
		tostring(tag),
	}

	if type(fields) == "table" then
		local keys = vim.tbl_keys(fields)
		table.sort(keys)

		for _, key in ipairs(keys) do
			table.insert(line, tostring(key) .. "=" .. encode_value(fields[key]))
		end
	elseif fields ~= nil then
		table.insert(line, encode_value(fields))
	end

	vim.fn.writefile({
		table.concat(line, " "),
	}, path, "a")
end

return M
