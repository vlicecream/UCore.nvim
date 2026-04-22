local client = require("ucore.client")
local maps = require("ucore.maps")
local project = require("ucore.project")
local remote = require("ucore.remote")
local server = require("ucore.server")
local ui = require("ucore.ui")
local bootstrap = require("ucore.bootstrap")

local M = {}

-- :UCore boot
-- 一键启动 UCore：server -> setup -> refresh -> watch。
function M.boot()
	bootstrap.boot(function(ok, err)
		if not ok and err then
			vim.notify("UCore boot failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end
	end)
end

-- :UCore start
-- 启动 Rust server。
function M.start()
	server.start(function(ok, message)
		if ok then
			vim.notify(message, vim.log.levels.INFO)
		else
			vim.notify(message, vim.log.levels.ERROR)
		end
	end)
end

-- :UCore stop
-- 停止当前 nvim 管理的 Rust server。
function M.stop()
	server.stop(function(ok, message)
		if ok then
			vim.notify(message, vim.log.levels.INFO)
		else
			vim.notify(message, vim.log.levels.ERROR)
		end
	end)
end

-- :UCore restart
-- 重启 Rust server。
function M.restart()
	server.restart(function(ok, message)
		if ok then
			vim.notify(message, vim.log.levels.INFO)
		else
			vim.notify(message, vim.log.levels.ERROR)
		end
	end)
end

-- Print command result or show an error notification.
-- 打印命令结果，或者显示错误通知。
local function notify_result(title, result, err)
	if err then
		vim.notify(title .. " failed:\n" .. tostring(err), vim.log.levels.ERROR)
		return
	end

	print(vim.inspect(result))
end

-- Build a setup/refresh payload for the current Unreal project.
-- 为当前 Unreal 工程构造 setup/refresh 请求体。
local function current_project_payload()
	local root = project.find_project_root()
	if not root then
		return nil, "Could not find .uproject"
	end

	local paths = project.build_paths(root)

	return {
		project_root = paths.project_root,
		db_path = paths.db_path,
		cache_db_path = paths.cache_db_path,
		config = project.default_config(),
		vcs_hash = nil,
	}
end

-- Check server status through the CLI bridge.
-- 通过 CLI 桥查询 server 状态。
function M.status()
	client.status(function(result, err)
		notify_result("UCore status", result, err)
	end)
end

-- Check server status through the direct TCP RPC client.
-- 通过 TCP RPC 直连查询 server 状态。
function M.rpc_status()
	client.rpc.request("status", {}, function(result, err)
		notify_result("UCore rpc-status", result, err)
	end)
end

-- Register the current Unreal project.
-- 注册当前 Unreal 工程。
function M.setup()
	local payload, err = current_project_payload()
	if err then
		return vim.notify(err, vim.log.levels.ERROR)
	end

	client.setup(payload, function(result, setup_err)
		notify_result("UCore setup", result, setup_err)
	end)
end

-- Refresh the current Unreal project index.
-- 刷新当前 Unreal 工程索引。
function M.refresh()
	local payload, err = current_project_payload()
	if err then
		return vim.notify(err, vim.log.levels.ERROR)
	end

	payload.type = "refresh"
	payload.engine_root = nil
	payload.scope = "Game"

	client.refresh(payload, function(result, refresh_err)
		notify_result("UCore refresh", result, refresh_err)
	end)
end

-- Pick indexed modules for the current Unreal project.
-- 选择当前 Unreal 工程的模块索引。
function M.modules()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	remote.get_modules(root, function(result, err)
		if err then
			return vim.notify("UCore modules failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		ui.select.modules(result or {})
	end)
end

-- Pick indexed assets for the current Unreal project.
-- 选择当前 Unreal 工程的资产索引。
function M.assets()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	remote.get_assets(root, function(result, err)
		if err then
			return vim.notify("UCore assets failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		ui.select.assets(result or {})
	end)
end

-- Search symbols and show them in a selection UI.
-- 搜索符号，并在选择 UI 中展示。
function M.search_symbols(pattern)
	pattern = vim.trim(pattern or "")

	if pattern == "" then
		return vim.notify("Usage: :UCore search-symbols <pattern>", vim.log.levels.WARN)
	end

	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	remote.search_symbols(root, pattern, function(result, err)
		if err then
			return vim.notify("UCore search-symbols failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		ui.select.symbols(result or {})
	end)
end

-- Print Lua-side component/module lookup maps.
-- 打印 Lua 侧整理后的 component/module 映射。
function M.maps()
	maps.get_maps(vim.api.nvim_buf_get_name(0), function(ok, result)
		if not ok then
			return vim.notify(tostring(result), vim.log.levels.ERROR)
		end

		print(vim.inspect(result))
	end)
end

-- Print :UCore command help.
-- 打印 :UCore 命令帮助。
function M.help()
	print([[
UCore commands:

  :UCore              Boot UCore for the current Unreal project
  :UCore boot         Start server, setup project, refresh if needed, and watch
  :UCore modules      Pick indexed modules
  :UCore assets       Pick indexed assets
  :UCore search-symbols <pattern>
                      Search indexed symbols
  :UCore debug help   Show debug commands
  :UCore help         Show this help
]])
end

-- Print :UCore debug command help.
-- 打印 :UCore debug 调试命令帮助。
function M.debug_help()
	print([[
UCore debug commands:

  :UCore debug status       Check server status through CLI bridge
  :UCore debug rpc-status   Check server status through direct TCP RPC
  :UCore debug start        Start Rust server
  :UCore debug stop         Stop server started by this nvim session
  :UCore debug restart      Restart Rust server
  :UCore debug setup        Register current Unreal project
  :UCore debug refresh      Refresh current Unreal project index
  :UCore debug maps         Print Lua-side component/module maps
  :UCore debug help         Show this help
]])
end

return M
