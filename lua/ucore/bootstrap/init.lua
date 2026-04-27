local client = require("ucore.client")
local config = require("ucore.config")
local project = require("ucore.project")
local server = require("ucore.server")
local status = require("ucore.status")

local M = {}

local booting = false
local engine_refreshing = {}

-- Report boot failures through the persistent initialization status.
-- 通过持久初始化状态报告 boot 错误。
local function fail(message, detail)
	status.fail("UCore initialization failed", detail or message)
end

-- Update the non-indexing part of boot progress.
-- 更新非索引部分的初始化进度。
local function other_progress(percent)
	status.progress(
		"UCore other initialization",
		string.format("UCore other initialization %d%%", percent)
	)
end

-- Build a setup/refresh/watch payload for the current Unreal project.
-- 为当前 Unreal 工程构造 setup/refresh/watch 请求体。
local function current_project_payload(project_root)
	local root = project_root or project.find_project_root()
	if not root then
		return nil, "Could not find .uproject"
	end

	project.register_project(root)

	local paths = project.build_paths(root)
	local engine, engine_err = project.engine_metadata(root)
	if not engine then
		return nil, engine_err
	end
	local engine_paths = project.build_engine_paths(engine)

	return {
		project_root = paths.project_root,
		db_path = paths.db_path,
		cache_db_path = paths.cache_db_path,
		config = project.default_config(),
		engine_root = engine.engine_root,
		vcs_hash = nil,
		_engine = engine,
		_engine_paths = engine_paths,
	}
end

-- Remove Lua-only fields before sending a request to Rust.
-- 发送给 Rust 前移除 Lua 内部字段。
local function rust_payload(payload)
	local copy = vim.deepcopy(payload)
	copy._engine = nil
	copy._engine_paths = nil
	return copy
end

-- Wait until the Rust server accepts RPC requests.
-- 等待 Rust server 可以接受 RPC 请求。
local function wait_ready(attempt, callback)
	attempt = attempt or 1

	client.rpc.request("status", {}, function(result, err)
		if not err and result and result.status == "running" then
			return callback(true, result)
		end

		if attempt >= config.values.boot_ready_attempts then
			return callback(false, err or "Server did not become ready")
		end

		vim.defer_fn(function()
			wait_ready(attempt + 1, callback)
		end, config.values.boot_ready_interval_ms)
	end)
end

-- Run setup and decide whether a full refresh is required.
-- 执行 setup，并判断是否需要 full refresh。
local function run_setup(payload, callback)
	client.setup(rust_payload(payload), function(result, err)
		if err then
			return callback(false, err)
		end

		callback(true, result or {})
	end)
end

-- Refresh the shared Unreal Engine index once per Engine install.
-- 按 Engine 安装维度刷新共享 Unreal Engine 索引。
local function run_engine_refresh_if_needed(payload, callback)
	local engine = payload._engine
	local engine_paths = payload._engine_paths

	if not engine or not engine_paths then
		status.progress_finish("UCore engine index", "UCore engine index 100%")
		return callback(true)
	end

	if not project.engine_needs_refresh(engine) then
		status.progress_finish("UCore engine index", "UCore engine index 100%")
		return callback(true)
	end

	if engine_refreshing[engine.engine_id] then
		status.progress_finish("UCore engine index", "UCore engine index 100%")
		return callback(true)
	end

	engine_refreshing[engine.engine_id] = true
	client.refresh({
		type = "refresh",
		project_root = engine.engine_root,
		engine_root = nil,
		db_path = engine_paths.db_path,
		cache_db_path = engine_paths.cache_db_path,
		config = project.default_config(),
		scope = "Game",
		vcs_hash = nil,
	}, function(_, err)
		engine_refreshing[engine.engine_id] = nil

		if err then
			return callback(false, err)
		end

		project.write_engine_index_metadata(engine)
		callback(true)
	end)
end

-- Refresh the shared Engine index after the project is already usable.
-- 在项目已经可用后，后台刷新共享 Engine 索引。
local function run_engine_refresh_in_background(payload)
	run_engine_refresh_if_needed(payload, function(ok, err)
		if ok then
			status.finish("UCore READY - initialization complete")
			return
		end

		status.fail("UCore engine index failed", tostring(err))
	end)
end

-- Run refresh when setup says the database is stale or missing.
-- 当 setup 判断数据库缺失或过期时执行 refresh。
local function run_refresh_if_needed(payload, setup_result, callback)
	if not setup_result.needs_full_refresh then
		status.progress_finish("UCore project index", "UCore project index 100%")
		return callback(true)
	end

	local refresh_payload = rust_payload(payload)
	refresh_payload.type = "refresh"
	refresh_payload.scope = "Game"

	client.refresh(refresh_payload, function(_, err)
		if err then
			return callback(false, err)
		end

		callback(true)
	end)
end

-- Start watcher after setup/refresh.
-- setup/refresh 后启动 watcher。
local function run_watch(payload, callback)
	client.watch({
		project_root = payload.project_root,
		db_path = payload.db_path,
	}, function(_, err)
		if err then
			return callback(false, err)
		end

		callback(true)
	end)
end

-- Boot the whole UCore stack for the current Unreal project.
-- 为当前 Unreal 工程一键启动完整 UCore 流程。
function M.boot(callback, opts)
	callback = callback or function() end
	opts = opts or {}

	if booting then
		status.start("UCore initializing...")
		return callback(false, "already booting")
	end

	local payload, err = current_project_payload(opts.project_root)
	if err then
		fail(err)
		return callback(false, err)
	end

	booting = true
	status.start("UCore initializing...")
	other_progress(0)

	server.start(function(ok, start_message)
		if not ok then
			booting = false
			fail(start_message)
			return callback(false, start_message)
		end
		other_progress(25)

		wait_ready(1, function(ready, ready_err)
			if not ready then
				booting = false
				fail(tostring(ready_err), "Log: " .. tostring(server.log_path()))
				return callback(false, ready_err)
			end
			other_progress(40)

			run_setup(payload, function(setup_ok, setup_result)
				if not setup_ok then
					booting = false
					fail(tostring(setup_result))
					return callback(false, setup_result)
				end
				other_progress(60)

				run_refresh_if_needed(payload, setup_result, function(refresh_ok, refresh_err)
					if not refresh_ok then
						booting = false
						fail(tostring(refresh_err))
						return callback(false, refresh_err)
					end

					run_watch(payload, function(watch_ok, watch_err)
						booting = false

						if not watch_ok then
							fail(tostring(watch_err))
							return callback(false, watch_err)
						end
						other_progress(100)

						callback(true)
						run_engine_refresh_in_background(payload)
					end)
				end)
			end)
		end)
	end)
end

return M
