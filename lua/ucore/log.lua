local config = require("ucore.config")
local server = require("ucore.server")

local M = {}
local uv = vim.uv or vim.loop
local ensured_dirs = {}

local function path_dirname(path)
	local normalized = tostring(path or ""):gsub("\\", "/")
	local parent = normalized:match("^(.*)/[^/]+$")
	return parent or ""
end

local function ensure_parent_dir(path)
	local parent = path_dirname(path)
	if parent == "" or ensured_dirs[parent] then
		return true
	end

	local stack = {}
	local current = parent

	while current ~= "" and not ensured_dirs[current] do
		local stat = uv.fs_stat(current)
		if stat and stat.type == "directory" then
			ensured_dirs[current] = true
			break
		end

		table.insert(stack, 1, current)
		local next_parent = path_dirname(current)
		if next_parent == current then
			break
		end
		current = next_parent
	end

	for _, dir in ipairs(stack) do
		local ok = uv.fs_mkdir(dir, 448)
		if not ok then
			local stat = uv.fs_stat(dir)
			if not (stat and stat.type == "directory") then
				return false
			end
		end

		ensured_dirs[dir] = true
	end

	return true
end

local function fallback_log_path()
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

local function sorted_keys(tbl)
	local keys = {}
	for key in pairs(tbl) do
		table.insert(keys, key)
	end
	table.sort(keys)
	return keys
end

function M.write(tag, fields)
	local ok, err = pcall(function()
		local path = active_log_path()
		if not ensure_parent_dir(path) then
			return
		end

		local line = {
			os.date("%Y-%m-%d %H:%M:%S"),
			tostring(tag),
		}

		if type(fields) == "table" then
			for _, key in ipairs(sorted_keys(fields)) do
				table.insert(line, tostring(key) .. "=" .. encode_value(fields[key]))
			end
		elseif fields ~= nil then
			table.insert(line, encode_value(fields))
		end

		local fd = uv.fs_open(path, "a", 420)
		if not fd then
			return
		end

		uv.fs_write(fd, table.concat(line, " ") .. "\n", -1)
		uv.fs_close(fd)
	end)

	if not ok and err then
		pcall(vim.schedule, function()
			vim.notify("UCore log write failed: " .. tostring(err), vim.log.levels.WARN)
		end)
	end
end

return M
