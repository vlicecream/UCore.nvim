local bootstrap = require("ucore.bootstrap")
local config = require("ucore.config")
local project = require("ucore.project")

local M = {}

local group_name = "UCoreAutoBoot"
local booted_projects = {}
local pending_projects = {}
local attempts_scheduled = false

local function buffer_allows_auto_boot(bufnr)
	local bo = vim.bo[bufnr]
	local path = vim.api.nvim_buf_get_name(bufnr)
	local filetype = bo.filetype

	if bo.buftype ~= "" or path == "" then
		return false
	end

	if filetype == "lazy" or filetype == "noice" or filetype == "checkhealth" then
		return false
	end

	return true
end

-- Schedule auto boot once per Unreal project root.
-- 对每个 Unreal 工程根目录只调度一次自动启动。
local function schedule_boot(project_root)
	if booted_projects[project_root] or pending_projects[project_root] then
		return
	end

	pending_projects[project_root] = true

	vim.defer_fn(function()
		pending_projects[project_root] = nil

		if booted_projects[project_root] then
			return
		end

		bootstrap.boot(function(ok, err)
			if ok then
				booted_projects[project_root] = true
			end
		end, {
			project_root = project_root,
		})
	end, config.values.auto_boot_delay_ms)
end

-- Try to auto boot from context. Retries up to 3 times with delays.
-- 从上下文尝试自动启动。最多重试 3 次，静默跳过。
local function try_auto_boot(args)
	if not config.values.auto_boot then
		return
	end

	local bufnr = args and args.buf or vim.api.nvim_get_current_buf()
	if not buffer_allows_auto_boot(bufnr) then
		return
	end

	if attempts_scheduled then
		return
	end

	attempts_scheduled = true
	local delays = { 0, 100, 300, 700 }
	local idx = 0

	local function tick()
		idx = idx + 1
		if idx > #delays then
			attempts_scheduled = false
			return
		end

		local buffer_path = vim.api.nvim_buf_get_name(bufnr)
		local root = project.find_project_root(buffer_path)
		if root then
			attempts_scheduled = false
			schedule_boot(root)
			return
		end

		if idx < #delays then
			vim.defer_fn(tick, delays[idx + 1] - delays[idx])
		else
			attempts_scheduled = false
		end
	end

	tick()
end

-- Register auto boot autocmds.
-- 注册自动启动 autocmd。
function M.setup()
	local group = vim.api.nvim_create_augroup(group_name, { clear = true })

	vim.api.nvim_create_autocmd(config.values.auto_boot_events, {
		group = group,
		callback = try_auto_boot,
	})

	vim.defer_fn(try_auto_boot, config.values.auto_boot_delay_ms)
end

-- Clear auto boot state, mostly useful for debugging.
-- 清理自动启动状态，主要用于调试。
function M.reset()
	booted_projects = {}
	pending_projects = {}
	attempts_scheduled = false
end

return M
