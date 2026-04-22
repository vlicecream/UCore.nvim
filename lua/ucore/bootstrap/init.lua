local client = require("ucore.client")
local project = require("ucore.project")
local server = require("ucore.server")

local M = {}

local booting = false

-- Notify with a consistent UCore prefix.
-- 使用统一的 UCore 前缀提示消息。
local function notify(message, level)
	vim.notify("UCore boot: " .. message, level or vim.log.levels.INFO)
end

-- Build a setup/refresh/watch payload for the current Unreal project.
-- 为当前 Unreal 工程构造 setup/refresh/watch 请求体。
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

-- Wait until the Rust server accepts RPC requests.
-- 等待 Rust server 可以接受 RPC 请求。
local function wait_ready(attempt, callback)
	attempt = attempt or 1

	client.rpc.request("status", {}, function(result, err)
		if not err and result and result.status == "running" then
			return callback(true, result)
		end

		if attempt >= 30 then
			return callback(false, err or "Server did not become ready")
		end

		vim.defer_fn(function()
			wait_ready(attempt + 1, callback)
		end, 100)
	end)
end

-- Run setup and decide whether a full refresh is required.
-- 执行 setup，并判断是否需要 full refresh。
local function run_setup(payload, callback)
	notify("setup")
	client.setup(payload, function(result, err)
		if err then
			return callback(false, err)
		end

		callback(true, result or {})
	end)
end

-- Run refresh when setup says the database is stale or missing.
-- 当 setup 判断数据库缺失或过期时执行 refresh。
local function run_refresh_if_needed(payload, setup_result, callback)
	if not setup_result.needs_full_refresh then
		notify("refresh skipped")
		return callback(true)
	end

	notify("refresh")
	local refresh_payload = vim.deepcopy(payload)
	refresh_payload.type = "refresh"
	refresh_payload.engine_root = nil
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
	notify("watch")
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
function M.boot(callback)
	callback = callback or function() end

	if booting then
		notify("already booting", vim.log.levels.WARN)
		return callback(false, "already booting")
	end

	local payload, err = current_project_payload()
	if err then
		notify(err, vim.log.levels.ERROR)
		return callback(false, err)
	end

	booting = true
	notify("starting server")

	server.start(function(ok, start_message)
		if not ok then
			booting = false
			notify(start_message, vim.log.levels.ERROR)
			return callback(false, start_message)
		end

		wait_ready(1, function(ready, ready_err)
			if not ready then
				booting = false
				notify(tostring(ready_err), vim.log.levels.ERROR)
				return callback(false, ready_err)
			end

			run_setup(payload, function(setup_ok, setup_result)
				if not setup_ok then
					booting = false
					notify(tostring(setup_result), vim.log.levels.ERROR)
					return callback(false, setup_result)
				end

				run_refresh_if_needed(payload, setup_result, function(refresh_ok, refresh_err)
					if not refresh_ok then
						booting = false
						notify(tostring(refresh_err), vim.log.levels.ERROR)
						return callback(false, refresh_err)
					end

					run_watch(payload, function(watch_ok, watch_err)
						booting = false

						if not watch_ok then
							notify(tostring(watch_err), vim.log.levels.ERROR)
							return callback(false, watch_err)
						end

						notify("ready")
						callback(true)
					end)
				end)
			end)
		end)
	end)
end

return M
