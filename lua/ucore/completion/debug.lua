local config = require("ucore.config")

local M = {}

local function debug_enabled()
	local completion_config = config.values.completion or {}
	return completion_config.debug == true
end

local function log_path()
	local data_dir = vim.fn.stdpath("data")
	vim.fn.mkdir(data_dir .. "/ucore", "p")
	return data_dir .. "/ucore/completion-debug.log"
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

return M
