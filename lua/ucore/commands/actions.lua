local project = require("ucore.project")
local remote = require("ucore.remote")
local server = require("ucore.server")
local ui = require("ucore.ui")
local bootstrap = require("ucore.bootstrap")
local navigation = require("ucore.navigation")
local explorer = require("ucore.explorer")
local install = require("ucore.install")

local M = {}
local FIND_PAGE_SIZE = 50
local find_cache = {}

local function show_find_results(pattern, items)
	ui.select.find(items, {
		default_text = pattern ~= "" and pattern or nil,
	})
end

local function copy_list(items)
	local result = {}
	for _, item in ipairs(items or {}) do
		table.insert(result, item)
	end
	return result
end

local function find_cache_snapshot(cache)
	return {
		initial_symbols = copy_list(cache.initial_symbols),
		static_items = copy_list(cache.static_items),
		ready = cache.ready == true,
		loading = cache.loading == true,
		errors = copy_list(cache.errors),
	}
end

local function notify_find_cache(cache)
	local snapshot = find_cache_snapshot(cache)
	local listeners = cache.listeners or {}
	cache.listeners = {}

	for _, callback in ipairs(listeners) do
		pcall(callback, snapshot)
	end
end

local function find_cache_for(root)
	root = tostring(root or "")
	if root == "" then
		return nil
	end

	local cache = find_cache[root]
	if cache then
		return cache
	end

	cache = {
		root = root,
		initial_symbols = {},
		static_items = {},
		errors = {},
		loading = false,
		ready = false,
		assets_loading = false,
		assets_ready = false,
		listeners = {},
	}
	find_cache[root] = cache
	return cache
end

local function append_modules(static_items, result)
	if type(result) ~= "table" then
		return
	end

	for _, module in ipairs(result or {}) do
		local path = module.build_cs_path or module.path
		if path and path ~= vim.NIL and path ~= "" then
			table.insert(static_items, {
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

local function append_assets(static_items, result)
	if type(result) ~= "table" then
		return
	end

	for _, asset in ipairs(result or {}) do
		local asset_path = type(asset) == "table" and (asset.path or asset.asset_path) or asset
		asset_path = tostring(asset_path or "")
		if asset_path ~= "" then
			table.insert(static_items, {
				name = vim.fn.fnamemodify(asset_path, ":t"),
				type = "asset",
				source = "project",
				asset_path = asset_path,
			})
		end
	end
end

local function append_config_data(static_items, result)
	if type(result) ~= "table" then
		return
	end

	for _, platform in ipairs(result or {}) do
		for _, section in ipairs(platform.sections or {}) do
			for _, param in ipairs(section.parameters or {}) do
				local history = param.history or {}
				local latest = history[#history] or {}
				table.insert(static_items, {
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

local function live_find_backend_query(query)
	query = vim.trim(tostring(query or ""))
	return query
end

local function live_find_is_identifier_query(query)
	query = tostring(query or "")
	if query == "" then
		return false
	end

	local first = query:sub(1, 1)
	if not first:match("[%a_]") then
		return false
	end

	return query:match("^[%a_][%w_:]*$") ~= nil
end

local function live_find_is_explicit_type_query(query)
	query = vim.trim(tostring(query or ""))
	if query == "" or not live_find_is_identifier_query(query) then
		return false
	end

	if query:find("::", 1, true) then
		return true
	end

	return query:sub(1, 1):match("%u") ~= nil
end

local function live_find_fallback_queries(query, scope)
	query = live_find_backend_query(query)
	local normalized = query:lower()
	if normalized:find("%s") then
		return {}
	end

	if scope == "engine" then
		return {}
	end

	local results = {}
	local seen = {}

	local function add(value)
		value = vim.trim(tostring(value or ""))
		if value == "" then
			return
		end

		local lowered = value:lower()
		if lowered == normalized or seen[lowered] or #lowered < 4 then
			return
		end

		seen[lowered] = true
		table.insert(results, value)
	end

	if #normalized >= 4 then
		add(query:sub(2))
	end

	if normalized:find("_", 1, true) then
		local compact = query:gsub("_+", "")
		add(compact)
		add(compact:sub(2))
	end

	return results
end

local function fetch_live_find(root, query, request, callback)
	local limit = request.limit or FIND_PAGE_SIZE
	local offset = request.offset or 0
	local query_limit = offset == 0 and math.max(limit, 120) or limit
	local fallback_limit = math.min(query_limit, 40)
	local primary = live_find_backend_query(query)
	local code_limit = math.min(math.max(limit, 80), 120)
	local should_run_text_stage = #primary >= 4 and not live_find_is_explicit_type_query(primary)

	local function collect(values)
		local results = {}
		for _, item in ipairs(values or {}) do
			table.insert(results, item)
		end
		return results
	end

	local stages = {
		project = { append = false, done = false, emitted = false, results = {}, errors = {} },
		engine = { append = true, done = false, emitted = false, results = {}, errors = {} },
	}

	local function add_all(target, values)
		for _, item in ipairs(values or {}) do
			table.insert(target, item)
		end
	end

	local function emit_stage(name, done)
		local stage = stages[name]
		if not stage or stage.emitted then
			return
		end

		stage.emitted = true
		if vim.tbl_isempty(stage.results) and not vim.tbl_isempty(stage.errors) then
			callback(nil, table.concat(stage.errors, "\n"), {
				append = stage.append == true,
				done = done == true,
			})
			return
		end

		callback(stage.results, nil, {
			append = stage.append == true,
			done = done == true,
		})
	end

	local function flush_stages()
		if stages.project.done and not stages.project.emitted then
			emit_stage("project", false)
		end

		if stages.project.emitted and stages.engine.done and not stages.engine.emitted then
			emit_stage("engine", true)
		end
	end

	local function run_fast(scope)
		local stage = stages[scope]
		local fallbacks = live_find_fallback_queries(query, scope)
		local pending = 1 + #fallbacks

		local function finish()
			pending = pending - 1
			if pending > 0 then
				return
			end

			stage.done = true
			flush_stages()
		end

		local function on_result(result, err)
			if err then
				table.insert(stage.errors, tostring(err))
			else
				add_all(stage.results, result)
			end
			finish()
		end

		remote.fast_find(root, primary, on_result, {
			limit = query_limit,
			offset = offset,
			scope = scope,
		})

		for _, fallback in ipairs(fallbacks) do
			remote.fast_find(root, fallback, on_result, {
				limit = fallback_limit,
				offset = offset,
				scope = scope,
			})
		end
	end

	if should_run_text_stage then
		remote.search_code_text(root, primary, function(result, err)
			if err then
				return callback(nil, err, { append = true, done = false })
			end

			callback(collect(result), nil, {
				append = true,
				done = false,
			})
		end, {
			limit = code_limit,
			offset = offset,
			scope = "project",
		})
	end

	run_fast("project")
	run_fast("engine")
end

local function subscribe_find_cache(root, callback)
	local cache = find_cache_for(root)
	if not cache or type(callback) ~= "function" then
		return
	end

	if cache.ready then
		local snapshot = find_cache_snapshot(cache)
		vim.schedule(function()
			callback(snapshot)
		end)
		return
	end

	table.insert(cache.listeners, callback)
end

local function load_find_assets(root, cache)
	cache = cache or find_cache_for(root)
	if not cache or cache.assets_loading or cache.assets_ready then
		return cache
	end

	cache.assets_loading = true

	remote.get_assets(root, function(result, err)
		cache.assets_loading = false
		cache.assets_ready = true

		if err then
			table.insert(cache.errors, tostring(err))
		else
			append_assets(cache.static_items, result)
		end

		notify_find_cache(cache)
	end)

	return cache
end

function M.prewarm_find(root, opts)
	opts = opts or {}
	local cache = find_cache_for(root)
	local include_assets = opts.include_assets == true
	if not cache then
		return cache
	end

	if cache.ready and include_assets and not cache.assets_ready then
		load_find_assets(root, cache)
	end

	if cache.loading or (cache.ready and not opts.force) then
		return cache
	end

	cache.loading = true
	cache.ready = false
	cache.initial_symbols = {}
	cache.static_items = {}
	cache.errors = {}
	cache.assets_loading = false
	cache.assets_ready = false

	local pending = 3
	local function finish()
		pending = pending - 1
		if pending > 0 then
			return
		end

		cache.loading = false
		cache.ready = true
		notify_find_cache(cache)

		if include_assets then
			load_find_assets(root, cache)
		end
	end

	remote.global_find(root, "", function(result, err)
		if err then
			table.insert(cache.errors, tostring(err))
		else
			cache.initial_symbols = type(result) == "table" and result or {}
		end
		finish()
	end, {
		limit = FIND_PAGE_SIZE,
		offset = 0,
	})

	remote.get_modules(root, function(result, err)
		if err then
			table.insert(cache.errors, tostring(err))
		else
			append_modules(cache.static_items, result)
		end
		finish()
	end)

	remote.get_config_data(root, function(result, err)
		if err then
			table.insert(cache.errors, tostring(err))
		else
			append_config_data(cache.static_items, result)
		end
		finish()
	end)

	return cache
end

local function current_project_label()
	local root = project.find_project_root_from_context()
	if not root then
		return "No Unreal project detected"
	end

	local name = vim.fn.fnamemodify(root, ":t")
	return string.format("%s - %s", name, root)
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

-- Open the left-side UCore Explorer tree.
-- 打开左侧 UCore Explorer 目录树。
function M.explorer()
	explorer.toggle()
end

-- :UCore goto <definition|references|source>
-- Smart goto, references, or source/header toggle.
-- 智能跳转、查找引用、源文件切换。
function M.goto(tail)
	local sub = (tail or ""):match("^%s*(%S+)")
	sub = sub and sub:lower() or ""

	local handlers = {
		["definition"] = navigation.goto_definition,
		["implementation"] = navigation.goto_implementation,
		["references"] = navigation.references,
		["source"] = navigation.toggle_source,
	}

	local handler = handlers[sub]
	if handler then
		handler()
		return
	end

	if sub == "" or sub == "help" then
		print([[
UCore goto subcommands:
  :UCore goto definition      Smart goto (gd)
  :UCore goto implementation  Go to implementation (gi)
  :UCore goto references      Find references (gr)
  :UCore goto source          Toggle between .cpp and .h (gs)
  :UCore goto help            Show this help
]])
		return
	end

	vim.notify("Unknown UCore goto subcommand: " .. sub .. "\nSee :UCore goto help", vim.log.levels.WARN)
end

-- Toggle between source (.cpp) and header (.h) file.
-- 在 .cpp 和 .h 文件之间切换。
function M.toggle_source()
	navigation.toggle_source()
end

-- Backward-compatible aliases used by dashboard.
-- Dashboard 使用的向后兼容别名。
function M.goto_definition()
	navigation.goto_definition()
end

function M.goto_implementation()
	navigation.goto_implementation()
end

function M.references()
	navigation.references()
end

function M.signature_help()
	require("ucore.assist").signature_help()
end

function M.blueprint()
	require("ucore.blueprint").show_related()
end

function M.rename(tail)
	local new_name = vim.trim(tostring(tail or ""))
	if new_name == "" then
		return require("ucore.assist").rename()
	end
	require("ucore.assist").rename(new_name)
end

function M.install(tail)
	install.run(tail)
end

-- Pick and open a registered Unreal project.
-- 选择并打开一个已注册 Unreal 项目。
function M.open_project()
	local items = project.list_registered_projects()

	if vim.tbl_isempty(items) then
		return vim.notify(
			"No registered UCore projects. Open a project once and let UCore register it automatically.",
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
	}

	if root then
		local metadata = registry.projects and registry.projects[root]
		state.registered = type(metadata) == "table"
		state.project_name = vim.fn.fnamemodify(root, ":t")

		local paths = project.build_paths(root)
		state.project_db_state = file_state(paths.db_path)
		state.cache_db_state = file_state(paths.cache_db_path)

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

-- Smart entry: pick project if outside, register+boot if unregistered, Dashboard if ready.
-- 智能入口：不在项目中选择，未注册则注册+boot，已注册则打开 Dashboard。
function M.smart_entry()
	-- Only use the current buffer for context. Scanning all open buffers
	-- causes false positives when another buffer is in a project but the
	-- current buffer is not.
	-- 只用当前 buffer 判断。扫描所有 buffer 会导致当前不在项目中时误判。
	local root = project.find_project_root()
	if not root then
		local items = project.list_registered_projects()
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
			label = "Open registered project",
			badge = registered_label(s),
			description = "Pick a known Unreal project",
			run = M.open_project,
		},
	}

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

-- Find indexed project items and show them in a selection UI.
-- 搜索已索引的项目内容，并在选择 UI 中展示。
function M.find(pattern)
	pattern = vim.trim(pattern or "")

	local root = project.find_project_root()
	if not root then
		return vim.notify("Could not find .uproject", vim.log.levels.ERROR)
	end

	local cache = M.prewarm_find(root, { include_assets = true })
	local snapshot = find_cache_snapshot(cache)
	local initial_symbols = pattern == "" and snapshot.initial_symbols or {}

	if ui.select.find_live then
		return ui.select.find_live(initial_symbols, {
			default_text = pattern ~= "" and pattern or nil,
			static_items = snapshot.static_items,
			page_size = FIND_PAGE_SIZE,
			initial_loading = snapshot.loading and pattern == "",
			subscribe_updates = function(callback)
				subscribe_find_cache(root, callback)
			end,
			fetch_symbols = function(query, request, callback)
				fetch_live_find(root, query, request, callback)
			end,
		})
	end

	local fallback_items = {}
	for _, item in ipairs(initial_symbols) do
		table.insert(fallback_items, item)
	end
	for _, item in ipairs(snapshot.static_items) do
		table.insert(fallback_items, item)
	end
	show_find_results(pattern, fallback_items)
end

local function format_verify_count(label, value)
	return string.format("  %s: %s", label, tostring(value or 0))
end

function M.verify()
	local root = project.find_project_root_from_context()
	if not root then
		local items = project.list_registered_projects()
		if vim.tbl_isempty(items) then
			return vim.notify(
				"UCore verify: not inside an Unreal project.\nOpen a .uproject file and run :UCore boot first.",
				vim.log.levels.WARN
			)
		end
		return vim.notify(
			"UCore verify: not inside an Unreal project.\nOpen a registered project first.",
			vim.log.levels.WARN
		)
	end

	local paths = project.build_paths(root)
	if vim.fn.filereadable(paths.db_path) ~= 1 then
		return vim.notify(
			"UCore verify: project index database is missing.\nRun :UCore boot first.",
			vim.log.levels.WARN
		)
	end

	remote.get_asset_index_status(root, function(result, err)
		if err then
			return vim.notify("UCore verify failed:\n" .. tostring(err), vim.log.levels.ERROR)
		end

		result = type(result) == "table" and result or {}
		local counts = type(result.counts) == "table" and result.counts or {}
		local issues = type(result.issues) == "table" and result.issues or {}
		local sample_assets = type(result.sample_assets) == "table" and result.sample_assets or {}
		local title = result.ok and "UCore verify: asset index looks healthy"
			or "UCore verify: asset index has issues"

		local lines = {
			title,
			"Project: " .. tostring(vim.fn.fnamemodify(root, ":t")),
			"DB: " .. tostring(paths.db_path),
			string.format(
				"Meta: db_version=%s asset_index_initialized=%s asset_index_version=%s expected=%s",
				tostring(result.db_version or "missing"),
				tostring(result.asset_index_initialized or "missing"),
				tostring(result.asset_index_version or "missing"),
				tostring(result.expected_asset_index_version or "unknown")
			),
			"Counts:",
			format_verify_count("assets", counts.assets),
			format_verify_count("asset_references", counts.asset_references),
			format_verify_count("asset_functions", counts.asset_functions),
			format_verify_count("blueprint_like_assets", counts.blueprint_like_assets),
			format_verify_count("orphan_asset_references", counts.orphan_asset_references),
			format_verify_count("orphan_asset_functions", counts.orphan_asset_functions),
		}

		if #issues > 0 then
			table.insert(lines, "Issues:")
			for _, item in ipairs(issues) do
				table.insert(lines, "  - " .. tostring(item))
			end
		end

		if #sample_assets > 0 then
			table.insert(lines, "Sample assets:")
			for _, item in ipairs(sample_assets) do
				table.insert(lines, "  - " .. tostring(item))
			end
		end

		vim.notify(table.concat(lines, "\n"), result.ok and vim.log.levels.INFO or vim.log.levels.WARN, {
			title = "UCore verify",
			timeout = result.ok and 8000 or 12000,
		})
	end)
end

-- Print :UCore command help.
-- 打印 :UCore 命令帮助。
function M.help()
	print([[
UCore commands:

  :UCore              Smart entry: boot, pick, or Dashboard
  :UCore boot         Boot current project, or pick a registered one
  :UCore explorer     Toggle the left-side Project/Source/Config tree
  :UCore find         Find indexed symbols, modules, assets, config
  :UCore verify       Verify current project asset index integrity
  :UCore goto         Navigation subcommands (see :UCore goto help)
  :UCore signature    Show signature help for current call
  :UCore blueprint    Show related Blueprint assets for symbol under cursor
  :UCore rename       Rename symbol under cursor
  :UCore install      Install Unreal source access plugin
  :UCore help         Show this help
]])
end

return M
