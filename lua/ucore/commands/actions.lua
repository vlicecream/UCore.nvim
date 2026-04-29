local client = require("ucore.client")
local config = require("ucore.config")
local maps = require("ucore.maps")
local project = require("ucore.project")
local remote = require("ucore.remote")
local server = require("ucore.server")
local ui = require("ucore.ui")
local unreal = require("ucore.unreal")
local bootstrap = require("ucore.bootstrap")
local completion = require("ucore.completion")
local navigation = require("ucore.navigation")
local vcs = require("ucore.vcs")

local M = {}

local FIND_CACHE_TTL_MS = 10000
local find_cache = {
	root = nil,
	items = nil,
	expires_at = 0,
}

local function now_ms()
	return vim.loop.hrtime() / 1000000
end

local function show_find_results(pattern, items)
	ui.select.find(items, {
		default_text = pattern ~= "" and pattern or nil,
	})
end

local function current_project_label()
	local root = project.find_project_root_from_context()
	if not root then
		return "No Unreal project detected"
	end

	local name = vim.fn.fnamemodify(root, ":t")
	return string.format("%s - %s", name, root)
end

local function yes_no(value)
	return value and "yes" or "no"
end

local function file_state(path)
	if not path or path == "" then
		return "missing"
	end

	if vim.fn.filereadable(path) == 1 then
		return "ok"
	end

	return "missing"
end

local function dir_state(path)
	if not path or path == "" then
		return "missing"
	end

	if vim.fn.isdirectory(path) == 1 then
		return "ok"
	end

	return "missing"
end

local function format_cmd(cmd)
	if type(cmd) ~= "table" then
		return tostring(cmd or "")
	end

	return table.concat(vim.tbl_map(tostring, cmd), " ")
end

local function open_scratch(title, lines)
	vim.cmd("botright new")
	local buf = vim.api.nvim_get_current_buf()
	vim.bo[buf].buftype = "nofile"
	vim.bo[buf].bufhidden = "wipe"
	vim.bo[buf].swapfile = false
	vim.bo[buf].filetype = "ucore-status"
	pcall(vim.api.nvim_buf_set_name, buf, "ucore://" .. title:gsub("%s+", "-"):lower() .. "/" .. tostring(buf))
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.bo[buf].modified = false
	return buf
end

local function latest_server_log()
	local candidates = {}
	local session_log = server.log_path()
	local fallback

	if session_log and session_log ~= "" then
		table.insert(candidates, session_log)
		fallback = session_log
	end

	local root = project.find_project_root()
	if root then
		local path = project.build_paths(root).log_path
		table.insert(candidates, path)
		fallback = fallback or path
	end

	for _, path in ipairs(vim.fn.glob(config.values.cache_dir .. "/**/u_core_server.log", false, true)) do
		table.insert(candidates, path)
	end

	local best
	local best_time = -1
	local seen = {}
	for _, path in ipairs(candidates) do
		path = tostring(path or "")
		if path ~= "" and not seen[path] and vim.fn.filereadable(path) == 1 then
			seen[path] = true
			local modified = vim.fn.getftime(path)
			if modified > best_time then
				best = path
				best_time = modified
			end
		end
	end

	return best or fallback
end

-- :UCore vcs login
-- Interactive P4 login.
-- 交互式 P4 登录。
function M.vcs_login()
  local ok, err = pcall(function()
    local p4 = require("ucore.vcs.p4")
    return p4.login()
  end)
  if ok then
    vim.notify("UCore: P4 login successful", vim.log.levels.INFO)
  else
    vim.notify("UCore: P4 login failed: " .. tostring(err), vim.log.levels.ERROR)
  end
end

-- :UCore vcs ...  dispatch
-- Dispatch VCS subcommands: dashboard, changes, checkout, commit, changelists, login.
-- 分发 VCS 子命令。
function M.vcs_dispatch(tail)
  local sub = (tail or ""):match("^%s*(%S+)") or "dashboard"
  sub = sub:lower()
  local handlers = {
    dashboard = function() require("ucore.vcs.dashboard").open() end,
    changes = M.changes,
    checkout = M.checkout,
    commit = M.commit,
    changelists = M.changelists,
    login = M.vcs_login,
  }
  local handler = handlers[sub]
  if handler then
    handler()
  else
    vim.notify("Unknown UCore vcs command: " .. sub .. "\nAvailable: dashboard changes checkout commit changelists login", vim.log.levels.WARN)
  end
  end

-- :UCore vcs dashboard
-- Open the VCS Dashboard.
-- 打开 VCS Dashboard。
function M.vcs_dashboard()
  vcs.open_dashboard("all")
end

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

-- Build the current Unreal Editor target and stream logs into a buffer.
-- 构建当前 Unreal Editor target，并实时输出日志到 buffer。
function M.build(args)
	unreal.build(args)
end

-- Cancel the currently running Unreal build.
-- 取消当前正在运行的 Unreal build。
function M.build_cancel()
	unreal.cancel_build()
end

-- Open the current Unreal project with its resolved Unreal Editor.
-- 使用解析到的 Unreal Editor 打开当前 Unreal 工程。
function M.editor(args)
	unreal.open_editor(args)
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

-- Collect current UCore state for dashboard display.
-- 收集当前 UCore 状态供 Dashboard 展示。
function M.collect_dashboard_state()
	local root = project.find_project_root_from_context()
	local registry = project.read_registry()
	local registered_count = vim.tbl_count(registry.projects or {})
	local engine_count = vim.tbl_count(registry.engines or {})

	local state = {
		project_root = root,
		project_name = nil,
		registered = false,
		server_running = server.is_running(),
		registered_count = registered_count,
		engine_count = engine_count,
		project_db_state = nil,
		cache_db_state = nil,
		engine_root = nil,
		engine_db_state = nil,
		log_exists = nil,
	}

	if root then
		local metadata = registry.projects and registry.projects[root]
		state.registered = type(metadata) == "table"
		state.project_name = vim.fn.fnamemodify(root, ":t")

		local paths = project.build_paths(root)
		state.project_db_state = file_state(paths.db_path)
		state.cache_db_state = file_state(paths.cache_db_path)
		state.log_exists = file_state(paths.log_path)

		local engine, _ = project.engine_metadata(root)
		if engine then
			state.engine_root = engine.engine_root
			local engine_paths = project.build_engine_paths(engine)
			state.engine_db_state = file_state(engine_paths.db_path)
		end
	end

	return state
end

-- Pad a string on the right using display width.
-- 按显示宽度右侧补空格。
local function pad_right(text, width)
	text = tostring(text or "")
	local padding = math.max(0, width - vim.fn.strdisplaywidth(text))
	return text .. string.rep(" ", padding)
end

-- Badge helpers — each returns a bracket label for one dashboard item.
-- Badge 辅助函数——每个返回一个方括号标签。

local function project_label(state)
	if not state.project_name then
		return "[no project]"
	end
	return "[" .. state.project_name .. "]"
end

local function index_label(state)
	if not state.project_root then
		return "[no project]"
	end
	if state.project_db_state ~= "ok" then
		return "[needs boot]"
	end
	return "[ready]"
end

local function cursor_label(state)
	if not state.project_root then
		return "[no project]"
	end
	return "[cursor symbol]"
end

local function build_label(state)
	if not state.project_root then
		return "[no project]"
	end
	return "[Win64 Development]"
end

local function editor_label(state)
	if not state.project_root then
		return "[no project]"
	end
	return "[build first]"
end

local function log_label(state)
	return (state.log_exists == "ok") and "[available]" or "[missing]"
end

local function registered_label(state)
	if state.registered_count > 0 then
		return "[" .. state.registered_count .. " registered]"
	end
	return "[none registered]"
end

-- Show a helpful message when not inside an Unreal project.
-- 当前不在 Unreal 项目时显示友好提示。
local function notify_no_project()
	local items = project.list_registered_projects()
	if vim.tbl_isempty(items) then
		vim.notify(
			"Not inside an Unreal project.\nOpen a .uproject file and run :UCore boot first.",
			vim.log.levels.WARN
		)
	else
		vim.notify(
			"Not inside an Unreal project.\nUse 'Open registered project' below or open a .uproject file.",
			vim.log.levels.WARN
		)
	end
end

-- Guard: the action needs an active Unreal project.
-- Guard：操作需要当前在 Unreal 项目中。
local function project_guard(fn)
	return function()
		local root = project.find_project_root_from_context()
		if root then
			return fn()
		end
		notify_no_project()
	end
end

-- Guard: the action needs both a project root and an existing index database.
-- Guard：操作需要项目根目录和已存在的索引数据库。
local function index_guard(fn)
	return function()
		local root = project.find_project_root_from_context()
		if not root then
			notify_no_project()
			return
		end

		local paths = project.build_paths(root)
		if vim.fn.filereadable(paths.db_path) ~= 1 then
			vim.notify(
				"Project index is missing.\nChoose 'Boot current project' from :UCore first.",
				vim.log.levels.WARN
			)
			return
		end

		fn()
	end
end

-- Format one dashboard item row with fixed-width columns.
-- 固定宽度列排版一次 dashboard item。
local function dashboard_format(item)
	return string.format(
		"%s  %s  - %s",
		pad_right(item.label, 28),
		pad_right(item.badge, 18),
		item.description
	)
end

-- Smart entry: boot new project, pick registered, or open Dashboard.
-- 三段式智能入口：新项目 boot，有注册项目选择，已注册项目打开 Dashboard。
function M.smart_entry()
	local root = project.find_project_root_from_context()
	if not root then
		local items = project.list_registered_projects()
		if vim.tbl_isempty(items) then
			vim.notify(
				"Not inside an Unreal project.\nOpen a .uproject file and run :UCore boot first.",
				vim.log.levels.WARN
			)
			return
		end
		ui.select.projects(items, function(item)
			if not item then
				return
			end
			project.open_project(item.root)
			M.boot()
		end)
		return
	end

	local registry = project.read_registry()
	local registered = registry.projects and registry.projects[root]
	if type(registered) ~= "table" then
		M.boot()
		return
	end

	M.dashboard()
end

-- Open the main UCore project dashboard.
-- 打开 UCore 项目主面板。
function M.dashboard()
	local s = M.collect_dashboard_state()

	local items = {
		{
			label = "Boot current project",
			badge = project_label(s),
			description = "Start server and prepare indexes",
			run = M.boot,
		},
		{
			label = "Find indexed items",
			badge = index_label(s),
			description = "Search symbols, modules, assets, config",
			run = index_guard(function()
				M.find("")
			end),
		},
		{
			label = "Go to definition",
			badge = cursor_label(s),
			description = "Jump from symbol under cursor",
			run = project_guard(M.goto_definition),
		},
		{
			label = "Find references",
			badge = cursor_label(s),
			description = "Find references for symbol under cursor",
			run = project_guard(M.references),
		},
		{
			label = "Build editor target",
			badge = build_label(s),
			description = "Build current Editor target",
			run = project_guard(function()
				M.build("")
			end),
		},
		{
			label = "Open Unreal Editor",
			badge = editor_label(s),
			description = "Build then launch Unreal Editor",
			run = project_guard(function()
				M.editor("")
			end),
		},
		{
			label = "Open registered project",
			badge = registered_label(s),
			description = "Pick a known Unreal project",
			run = M.open_project,
		},
	}

	if s.project_root then
		local _, project_vcs = pcall(vcs.detect, s.project_root)
		if project_vcs then
			-- Source Control dashboard items (only P4-specific extras)
			if project_vcs.name() == "p4" then
				vim.list_extend(items, {
					{
						label = "Source Control: Pending changelists",
						badge = "[P4]",
						description = "View pending P4 changelists",
						run = M.changelists,
					},
				})
			end
		end
	end

	-- Always add global source control items
	vim.list_extend(items, {
		{
			label = "Source Control: VCS Dashboard",
			badge = "[VCS]",
			description = "Open LazyGit-style VCS Dashboard",
			run = M.vcs_dashboard,
		},
		{
			label = "Source Control: Open changes",
			badge = "[VCS]",
			description = "List changed files",
			run = M.changes,
		},
		{
			label = "Source Control: Checkout current file",
			badge = "[VCS]",
			description = "p4 edit for current buffer",
			run = M.checkout,
		},
		{
			label = "Source Control: Commit changes",
			badge = "[VCS]",
			description = "Open visual commit UI",
			run = M.commit,
		},
	})

	ui.select.items("UCore dashboard", items, {
		format_item = dashboard_format,
		on_choice = function(item)
			if item and item.run then
				item.run()
			end
		end,
	})
end

-- :UCore boot
-- Smart entrypoint: boot current project or pick a registered one.
-- 智能入口：启动当前项目，或选择一个已注册项目。
function M.boot()
	local root = project.find_project_root_from_context()

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
-- 打开用户可读的 UCore 状态面板。
function M.status()
	local root = project.find_project_root()
	local registry = project.read_registry()
	local lines = {
		"UCore Status",
		"",
		"Server",
		"  managed by this nvim: " .. yes_no(server.is_running()),
		"  port: " .. tostring(config.values.port),
		"  backend mode: " .. tostring(config.values.backend_mode),
		"  scanner command: " .. format_cmd(config.values.scanner_cmd),
		"  server command: " .. format_cmd(config.values.server_cmd),
		"",
		"Cache",
		"  cache dir: " .. tostring(config.values.cache_dir),
		"  cache dir state: " .. dir_state(config.values.cache_dir),
		"  registry: " .. project.global_registry_path(),
		"  server registry: " .. project.server_registry_path(),
		"  registered projects: " .. tostring(vim.tbl_count(registry.projects or {})),
		"  registered engines: " .. tostring(vim.tbl_count(registry.engines or {})),
	}

	if root then
		local paths = project.build_paths(root)
		local metadata = registry.projects and registry.projects[root]
		local engine, engine_err = project.engine_metadata(root)

		vim.list_extend(lines, {
			"",
			"Current Project",
			"  root: " .. root,
			"  registered: " .. yes_no(type(metadata) == "table"),
			"  db: " .. paths.db_path .. " [" .. file_state(paths.db_path) .. "]",
			"  cache db: " .. paths.cache_db_path .. " [" .. file_state(paths.cache_db_path) .. "]",
			"  server log: " .. paths.log_path .. " [" .. file_state(paths.log_path) .. "]",
		})

		if engine then
			local engine_paths = project.build_engine_paths(engine)
			vim.list_extend(lines, {
				"",
				"Unreal Engine",
				"  association: " .. tostring(engine.engine_association or ""),
				"  root: " .. tostring(engine.engine_root or ""),
				"  id: " .. tostring(engine.engine_id or ""),
				"  needs refresh: " .. yes_no(project.engine_needs_refresh(engine)),
				"  db: " .. engine_paths.db_path .. " [" .. file_state(engine_paths.db_path) .. "]",
				"  cache db: " .. engine_paths.cache_db_path .. " [" .. file_state(engine_paths.cache_db_path) .. "]",
			})
		else
			vim.list_extend(lines, {
				"",
				"Unreal Engine",
				"  error: " .. tostring(engine_err),
			})
		end
	else
		vim.list_extend(lines, {
			"",
			"Current Project",
			"  root: not inside an Unreal project",
			"  tip: run :UCore to choose a registered project",
		})
	end

	local buf = open_scratch("UCore Status", lines)

	client.rpc.request("status", {}, function(result, err)
		if not vim.api.nvim_buf_is_valid(buf) then
			return
		end

		local rpc_lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
		vim.list_extend(rpc_lines, {
			"",
			"RPC",
			err and ("  status: offline [" .. tostring(err) .. "]") or "  status: online",
		})

		if not err then
			vim.list_extend(rpc_lines, vim.split(vim.inspect(result), "\n", { plain = true }))
		end

		vim.api.nvim_buf_set_lines(buf, 0, -1, false, rpc_lines)
		vim.bo[buf].modified = false
	end)
end

-- Open the latest known UCore server log.
-- 打开最近的 UCore server 日志。
function M.logs()
	local path = latest_server_log()

	if not path then
		return vim.notify("UCore logs: no server log found yet", vim.log.levels.WARN)
	end

	if vim.fn.filereadable(path) ~= 1 then
		vim.fn.mkdir(vim.fn.fnamemodify(path, ":p:h"), "p")
		vim.fn.writefile({
			"UCore server log",
			"Log file created by :UCore logs.",
			"Start UCore with :UCore to collect server output here.",
			"",
		}, path)
	end

	vim.cmd.edit(vim.fn.fnameescape(path))
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

-- Find indexed project items and show them in a selection UI.
-- 搜索已索引的项目内容，并在选择 UI 中展示。
function M.find(pattern)
	pattern = vim.trim(pattern or "")

	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	if find_cache.root == root and find_cache.items and find_cache.expires_at > now_ms() then
		return show_find_results(pattern, find_cache.items)
	end

	local pending = 4
	local items = {}
	local errors = {}

	local function append_many(values)
		for _, item in ipairs(values or {}) do
			table.insert(items, item)
		end
	end

	local function finish()
		pending = pending - 1
		if pending > 0 then
			return
		end

		if vim.tbl_isempty(items) and not vim.tbl_isempty(errors) then
			return vim.notify("UCore find failed:\n" .. table.concat(errors, "\n"), vim.log.levels.ERROR)
		end

		find_cache = {
			root = root,
			items = items,
			expires_at = now_ms() + FIND_CACHE_TTL_MS,
		}
		show_find_results(pattern, items)
	end

	remote.search_symbols(root, "", function(result, err)
		if err then
			table.insert(errors, tostring(err))
		else
			append_many(result or {})
		end
		finish()
	end, 5000)

	remote.get_modules(root, function(result, err)
		if err then
			table.insert(errors, tostring(err))
		else
			for _, module in ipairs(result or {}) do
				local path = module.build_cs_path or module.path
				if path and path ~= vim.NIL and path ~= "" then
					table.insert(items, {
						name = module.name,
						type = "module",
						source = "project",
						path = path,
						module_name = module.name,
						class_name = module.owner_name or module.component_name,
					})
				end
			end
		end
		finish()
	end)

	remote.get_assets(root, function(result, err)
		if err then
			table.insert(errors, tostring(err))
		else
			for _, asset in ipairs(result or {}) do
				local asset_path = type(asset) == "table" and (asset.path or asset.asset_path) or asset
				asset_path = tostring(asset_path or "")
				if asset_path ~= "" then
					table.insert(items, {
						name = vim.fn.fnamemodify(asset_path, ":t"),
						type = "asset",
						source = "project",
						asset_path = asset_path,
					})
				end
			end
		end
		finish()
	end)

	remote.get_config_data(root, function(result, err)
		if err then
			table.insert(errors, tostring(err))
		else
			for _, platform in ipairs(result or {}) do
				for _, section in ipairs(platform.sections or {}) do
					for _, param in ipairs(section.parameters or {}) do
						local history = param.history or {}
						local latest = history[#history] or {}
						table.insert(items, {
							name = tostring(param.key or ""),
							type = "config",
							source = tostring(platform.platform or platform.name or "config"),
							path = latest.full_path,
							line = latest.line,
							config_section = section.name,
							config_value = param.value,
							config_file = latest.file,
						})
					end
				end
			end
		end
		finish()
	end)
end

-- Backward-compatible debug alias for the old command name.
-- 旧命令名保留为 debug 兼容入口。
function M.search_symbols(pattern)
	M.find(pattern)
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

-- :UCore changelists
-- Show pending P4 changelists (independent list).
-- 显示 P4 pending changelist（独立列表）。
function M.changelists()
  local root = project.find_project_root()
  if not root then
    return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
  end
  require("ucore.vcs.changelists").list(root)
end

-- :UCore changes
-- Show VCS changes via the unified LazyGit-style dashboard (filtered to file rows).
-- 通过统一的 LazyGit 风格 dashboard 显示改动文件列表。
function M.changes()
  vcs.open_dashboard("files")
end

-- :UCore commit
-- Open the VCS commit UI (scratch buffer with file list + message).
-- 打开 VCS 可视化提交界面（scratch buffer，含文件列表和提交说明）。
function M.commit()
  vcs.open_commit_ui(nil, nil)
end

-- :UCore checkout
-- Checkout the current buffer file via VCS (p4 edit).
-- 对当前文件执行 VCS checkout（P4 下为 p4 edit）。
function M.checkout()
  local path = vim.api.nvim_buf_get_name(0)
  if path == "" then
    return vim.notify("UCore: no file in current buffer", vim.log.levels.WARN)
  end

  local provider = vcs.detect_for_path(path)
  if not provider then
    return vim.notify("UCore: no VCS provider detected for this file", vim.log.levels.WARN)
  end

  if provider.name() ~= "p4" then
    return vim.notify("UCore: checkout is a no-op for " .. provider.name():upper(), vim.log.levels.INFO)
  end

  local ok, err = provider.checkout(path)
  if ok then
    vim.bo[0].readonly = false
    vim.notify("UCore: p4 edit " .. vim.fn.fnamemodify(path, ":t"), vim.log.levels.INFO)
  else
    vim.notify("UCore checkout failed: " .. tostring(err), vim.log.levels.ERROR)
  end
end

-- :UCore debug p4-changes
-- Print pending P4 changelists.
-- 打印 P4 pending changelist 简表。
function M.pending_changelists()
  local root = project.find_project_root()
  if not root then
    return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
  end
  local provider = vcs.detect(root)
  if not provider or provider.name() ~= "p4" then
    return vim.notify("UCore: no P4 provider detected", vim.log.levels.WARN)
  end
  if not provider.pending_changelists then
    return vim.notify("UCore: pending_changelists not available", vim.log.levels.WARN)
  end
  local changes = provider.pending_changelists(root)
  if #changes == 0 then
    return vim.notify("UCore: no pending changelists", vim.log.levels.INFO)
  end
  print(vim.inspect(changes))
end

-- :UCore debug vcs
-- Print VCS diagnostics for the current file/project.
-- 打印当前文件/项目的 VCS 诊断信息。
function M.vcs_debug()
  local path = vim.api.nvim_buf_get_name(0)
  local root = project.find_project_root(path)

  local lines = {
    "UCore VCS Debug",
    "",
    "Current buffer: " .. (path ~= "" and path or "(none)"),
    "Project root: " .. tostring(root or "(not in Unreal project)"),
  }

  if root then
    local provider = vcs.detect(root)
    if provider then
      lines[#lines + 1] = "Provider: " .. provider.name():upper()

      if provider.name() == "p4" then
        lines[#lines + 1] = "P4 config source: " .. tostring(provider.config_source())
        lines[#lines + 1] = "P4 command: " .. tostring(provider.p4_cmd("info")[1])

        local info, info_err = provider.info(root)
        if info then
          lines[#lines + 1] = "P4 client: " .. tostring(info["client name"] or "?")
          lines[#lines + 1] = "P4 user: " .. tostring(info["user name"] or "?")
          lines[#lines + 1] = "P4 root: " .. tostring(info["client root"] or "?")
          lines[#lines + 1] = "P4 server: " .. tostring(info["server address"] or "?")
        else
          lines[#lines + 1] = "P4 info: " .. tostring(info_err)
        end

        if path and path ~= "" then
          lines[#lines + 1] = ""
          lines[#lines + 1] = "Current file:"
          lines[#lines + 1] = "  writable: " .. tostring(vim.fn.filewritable(path) == 1)
          lines[#lines + 1] = "  buffer readonly: " .. tostring(vim.bo[0].readonly)
          local opened = provider.is_opened(path)
          lines[#lines + 1] = "  p4 opened: " .. tostring(opened)
        end

        lines[#lines + 1] = ""
        lines[#lines + 1] = "All opened files:"
        local opened_files = provider.opened(root)
        if #opened_files > 0 then
          for _, f in ipairs(opened_files) do
            lines[#lines + 1] = "  [" .. f.action .. "] " .. f.path
          end
        else
          lines[#lines + 1] = "  (none)"
        end
      end
    else
      lines[#lines + 1] = "Provider: none (not in a VCS workspace)"
    end
  end

  vim.cmd("botright new")
  local buf = vim.api.nvim_get_current_buf()
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "ucore-status"
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modified = false
end

-- Print :UCore command help.
-- 打印 :UCore 命令帮助。
function M.help()
	print([[
UCore commands:

  :UCore              Smart entry: boot, pick, or Dashboard
  :UCore boot         Boot current project, or pick a registered one
  :UCore build        Build current Unreal Editor target
  :UCore build-cancel Cancel the currently running Unreal build
  :UCore editor       Open current project in Unreal Editor
  :UCore find         Find indexed symbols, modules, assets, config
   :UCore goto         Go to definition at cursor
  :UCore references   Find references at cursor
   :UCore vcs           Open LazyGit-style VCS Dashboard
  :UCore vcs dashboard  Open VCS Dashboard
  :UCore vcs changes    Show file changes in picker
  :UCore vcs changelists View pending P4 changelists
  :UCore vcs checkout   Checkout current file (p4 edit)
  :UCore vcs commit     Open visual commit UI
  :UCore changes       Show VCS changes
  :UCore checkout      Checkout current file (p4 edit)
  :UCore commit        Open visual commit UI
  :UCore changelists   View pending P4 changelists
  :UCore debug        Debug and lifecycle subcommands
  :UCore help         Show this help
]])
end

-- Print :UCore debug command help.
-- 打印 :UCore debug 调试命令帮助。
function M.debug_help()
	print([[
UCore debug commands:

  :UCore debug logs         Open the latest UCore server log
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
  :UCore debug vcs          Print VCS diagnostics
  :UCore debug p4-changes   Print pending P4 changelists
  :UCore debug help         Show this help
]])
end

return M
