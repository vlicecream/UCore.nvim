local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

local group_name = "UCoreAutosave"
local pending = {}

local function autosave_interval_ms()
	local seconds = tonumber(config.values.autosave) or 0
	if seconds <= 0 then
		return 0
	end

	return math.floor(seconds * 1000)
end

local function buffer_allows_autosave(bufnr)
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return false
	end

	local bo = vim.bo[bufnr]
	local path = vim.api.nvim_buf_get_name(bufnr)
	if bo.buftype ~= "" or path == "" then
		return false
	end

	if bo.modifiable == false or bo.readonly == true then
		return false
	end

	if bo.modified ~= true then
		return false
	end

	return project.find_project_root(path) ~= nil
end

local function save_buffer(bufnr)
	if not buffer_allows_autosave(bufnr) then
		return false
	end

	local ok = pcall(vim.api.nvim_buf_call, bufnr, function()
		vim.cmd("silent keepalt update")
	end)

	return ok
end

local function schedule_save(bufnr)
	local delay_ms = autosave_interval_ms()
	if delay_ms <= 0 then
		return
	end

	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	pending[bufnr] = (pending[bufnr] or 0) + 1
	local token = pending[bufnr]

	vim.defer_fn(function()
		if pending[bufnr] ~= token then
			return
		end

		if not vim.api.nvim_buf_is_valid(bufnr) then
			pending[bufnr] = nil
			return
		end

		vim.schedule(function()
			if not vim.api.nvim_buf_is_valid(bufnr) then
				pending[bufnr] = nil
				return
			end
			save_buffer(bufnr)
		end)
	end, delay_ms)
end

function M.setup()
	local group = vim.api.nvim_create_augroup(group_name, { clear = true })

	vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI" }, {
		group = group,
		pattern = "*",
		callback = function(args)
			if autosave_interval_ms() <= 0 then
				return
			end

			schedule_save(args.buf)
		end,
	})

	vim.api.nvim_create_autocmd({ "BufWritePost", "BufDelete", "BufWipeout" }, {
		group = group,
		pattern = "*",
		callback = function(args)
			pending[args.buf] = nil
		end,
	})
end

function M.reset()
	pending = {}
	pcall(vim.api.nvim_del_augroup_by_name, group_name)
end

return M
