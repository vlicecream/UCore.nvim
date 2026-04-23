local client = require("ucore.client")
local maps = require("ucore.maps")
local project = require("ucore.project")
local remote = require("ucore.remote")
local server = require("ucore.server")
local ui = require("ucore.ui")
local bootstrap = require("ucore.bootstrap")
local completion = require("ucore.completion")
local navigation = require("ucore.navigation")

local M = {}

-- Jump to definition at the current cursor.
-- 跳转当前光标下符号的定义。
function M.goto_definition()
	navigation.goto_definition()
end

-- Find references for the symbol at the current cursor.
-- 查找当前光标下符号的引用。
function M.references()
	navigation.references()
end

-- Print the resolved Unreal Engine root for the current project.
-- 打印当前项目解析到的 Unreal Engine 根目录。
function M.engine()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local engine, err = project.engine_metadata(root)
	if not engine then
		return vim.notify(tostring(err), vim.log.levels.ERROR)
	end

	local paths = project.build_engine_paths(engine)
	print(vim.inspect(vim.tbl_extend("force", engine, {
		db_path = paths.db_path,
		cache_db_path = paths.cache_db_path,
		needs_refresh = project.engine_needs_refresh(engine),
	})))
end

-- Trigger manual completion through Rust completion engine.
-- 通过 Rust 补全引擎触发手动补全。
function M.complete()
	completion.complete()
end

-- Register the current Unreal project in the global registry.
-- 将当前 Unreal 项目注册到全局注册表。
function M.register_project()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local metadata = project.register_project(root)
	vim.notify("UCore registered project: " .. metadata.name, vim.log.levels.INFO)
end

-- Pick and open a registered Unreal project.
-- 选择并打开一个已注册 Unreal 项目。
function M.open_project()
	local items = project.list_registered_projects()

	if vim.tbl_isempty(items) then
		return vim.notify(
			"No registered UCore projects. Open a project and run :UCore register first.",
			vim.log.levels.WARN
		)
	end

	ui.select.projects(items, function(item)
		if not item then
			return
		end

		project.open_project(item.root)
		M.boot()
	end)
end

-- Print registered projects.
-- 打印已注册项目。
function M.projects()
	print(vim.inspect(project.list_registered_projects()))
end

-- :UCore boot
-- Smart entrypoint: boot current project or pick a registered one.
-- 智能入口：启动当前项目，或选择一个已注册项目。
function M.boot()
	local root = project.find_project_root()

	if root then
		project.register_project(root)

		return bootstrap.boot(function(ok, err)
			if not ok and err then
				vim.notify("UCore boot failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end
		end, {
			project_root = root,
		})
	end

	local items = project.list_registered_projects()
	if vim.tbl_isempty(items) then
		return vim.notify(
			"UCore: no Unreal project found and no registered projects.\nOpen a .uproject project once, then run :UCore.",
			vim.log.levels.WARN
		)
	end

	ui.select.projects(items, function(item)
		if not item then
			return
		end

		project.open_project(item.root)

		bootstrap.boot(function(ok, err)
			if not ok and err then
				vim.notify("UCore boot failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end
		end, {
			project_root = item.root,
		})
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

-- Force-refresh the shared Unreal Engine index for the current project.
-- 强制刷新当前项目对应的共享 Unreal Engine 索引。
function M.engine_refresh()
	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local engine, err = project.engine_metadata(root)
	if not engine then
		return vim.notify(tostring(err), vim.log.levels.ERROR)
	end

	local paths = project.build_engine_paths(engine)
	vim.notify("UCore engine refresh: " .. engine.engine_root, vim.log.levels.INFO)

	client.refresh({
		type = "refresh",
		project_root = engine.engine_root,
		engine_root = nil,
		db_path = paths.db_path,
		cache_db_path = paths.cache_db_path,
		config = project.default_config(),
		scope = "Game",
		vcs_hash = nil,
	}, function(result, refresh_err)
		if refresh_err then
			return notify_result("UCore engine refresh", result, refresh_err)
		end

		project.write_engine_index_metadata(engine)
		notify_result("UCore engine refresh", {
			engine_id = engine.engine_id,
			engine_root = engine.engine_root,
			db_path = paths.db_path,
			status = "ok",
		}, nil)
	end)
end

-- Build a setup/refresh payload for the current Unreal project.
-- 为当前 Unreal 工程构造 setup/refresh 请求体。
local function current_project_payload()
	local root = project.find_project_root()
	if not root then
		return nil, "Could not find .uproject"
	end

	local engine, engine_err = project.engine_metadata(root)
	if not engine then
		return nil, engine_err
	end

	local paths = project.build_paths(root)

	return {
		project_root = paths.project_root,
		db_path = paths.db_path,
		cache_db_path = paths.cache_db_path,
		config = project.default_config(),
		engine_root = engine.engine_root,
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

  :UCore              Open or boot an Unreal project
  :UCore boot         Same as :UCore
  :UCore debug help   Show debug commands
  :UCore help         Show this help
  :UCore goto         Go to definition at cursor
  :UCore references   Find references at cursor
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
  :UCore debug register     Register current project in global registry
  :UCore debug open         Pick and open a registered project
  :UCore debug projects     Print registered projects
  :UCore debug engine       Show resolved Unreal Engine root/cache
  :UCore debug engine-refresh
                            Force refresh shared Unreal Engine index
  :UCore debug modules      Pick indexed modules
  :UCore debug assets       Pick indexed assets
  :UCore debug search-symbols <pattern>
                            Search indexed symbols
  :UCore debug goto         Go to definition at cursor
  :UCore debug references   Find references at cursor
  :UCore debug complete     Trigger manual completion in Insert mode
  :UCore debug maps         Print Lua-side component/module maps
  :UCore debug help         Show this help
]])
end

return M
