local backend = require("ucore.backend")
local client = require("ucore.client")
local config = require("ucore.config")
local log = require("ucore.log")
local protocol = require("ucore.protocol")
local project = require("ucore.project")
local server = require("ucore.server")
local status = require("ucore.status")

local M = {}

local booting = false
local engine_refreshing = {}
local ready_projects = {}

local function project_key(project_root)
	return tostring(project_root or "")
end

local function set_project_ready(project_root, ready)
	local key = project_key(project_root)
	if key == "" then
		return
	end

	if ready then
		ready_projects[key] = true
	else
		ready_projects[key] = nil
	end
end

-- Report boot failures through the persistent initialization status.
-- 通过持久初始化状态报告 boot 错误。
local function fail(message, detail)
	log.write_progress("boot-fail", {
		message = message,
		detail = detail,
	})
	status.fail("UCore Initialization Failed", detail or message)
end

local function server_step(title, percent, detail, done)
	log.write_progress("boot-server-step", {
		title = title,
		percent = percent,
		detail = detail,
	})
	local message = string.format("%s %d%%", title, percent)
	if done or percent >= 100 then
		status.progress_finish(title, message)
		return
	end

	status.progress(title, message)
end

local function server_start_progress(percent, detail)
	server_step("UCore Server Start", percent, detail, percent >= 100)
end

local function server_ready_progress(percent, detail)
	server_step("UCore Server Ready", percent, detail, percent >= 100)
end

local function workspace_register_progress(percent, detail)
	server_step("UCore Workspace Register", percent, detail, percent >= 100)
end

local function project_code_progress(percent, detail)
	log.write_progress("boot-project", {
		percent = percent,
		detail = detail,
	})
	local title = "UCore Project Discovery"
	local message = string.format("UCore Project Discovery %d%%", percent)
	if percent >= 100 then
		status.progress_finish(title, message)
		return
	end

	status.progress(title, message)
end

local function project_finalize_progress(percent, detail)
	log.write_progress("boot-project-finalize", {
		percent = percent,
		detail = detail,
	})
	local title = "UCore Project Finalize"
	local message = string.format("UCore Project Finalize %d%%", percent)
	if percent >= 100 then
		status.progress_finish(title, message)
		return
	end

	status.progress(title, message)
end

local function finish_phase(title)
	status.progress_finish(title, string.format("%s 100%%", title))
end

local function finish_phase_list(titles)
	for _, title in ipairs(titles or {}) do
		finish_phase(title)
	end
end

local function finish_project_skip_phases()
	finish_phase_list({
		"UCore Project Discovery",
		"UCore Project DB Prepare",
		"UCore Project Analysis",
		"UCore Project DB Write",
		"UCore Project Text DB Write",
		"UCore Project Asset Scan",
		"UCore Project Asset Persist",
	})
end

local function finish_engine_skip_phases()
	finish_phase_list({
		"UCore Engine Discovery",
		"UCore Engine DB Prepare",
		"UCore Engine Analysis",
		"UCore Engine DB Write",
		"UCore Engine Text DB Write",
		"UCore Engine Finalize",
		"UCore Engine Asset Scan",
		"UCore Engine Asset Persist",
	})
end

local function finish_engine_status(message)
	if message and message ~= "" then
		status.progress_finish("UCore Engine Discovery", message)
	else
		status.progress_finish("UCore Engine Discovery", "UCore Engine Discovery 100%")
	end
end

local function fail_engine_status(message)
	status.progress_fail("UCore Engine Discovery", message or "UCore Engine Discovery Failed")
end

local function refresh_active_buffer_diagnostics()
	vim.schedule(function()
		local ok, diagnostics = pcall(require, "ucore.diagnostics")
		if not ok or not diagnostics or type(diagnostics.refresh) ~= "function" then
			return
		end

		local bufnr = vim.api.nvim_get_current_buf()
		if not bufnr or bufnr == 0 or not vim.api.nvim_buf_is_valid(bufnr) then
			return
		end

		diagnostics.refresh(bufnr, {
			force = true,
			silent = true,
			errors_only = false,
		})
	end)
end

-- Build a setup/refresh/watch payload for the current Unreal project.
-- 为当前 Unreal 工程构造 setup/refresh/watch 请求体。
local function current_project_payload(project_root)
	local root = project_root or project.find_project_root_from_context()
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
	local copy = {}
	for key, value in pairs(payload) do
		if key ~= "_engine" and key ~= "_engine_paths" then
			copy[key] = value
		end
	end
	return copy
end

-- Wait until the Rust server accepts RPC requests.
-- 等待 Rust server 可以接受 RPC 请求。
local function wait_ready(attempt, callback)
	attempt = attempt or 1

	client.status(function(result, err)
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

local function wait_compatible(payload, replaced, callback)
	wait_ready(1, function(ready, result)
		if not ready then
			return callback(false, result)
		end

		if protocol.is_compatible(result) then
			return callback(true, result)
		end

		if replaced then
			return callback(false, protocol.compatibility_error(result))
		end

		server_start_progress(60, "Replacing server...")
		local function replace_server()
			server.replace(function(ok, replace_message)
				if not ok then
					return callback(false, replace_message)
				end

				server_start_progress(100, "Server replaced.")
				wait_compatible(payload, true, callback)
			end, {
				project_root = payload.project_root,
			})
		end

		if not backend.can_update_managed_backend() then
			return replace_server()
		end

		status.progress("UCore Backend Build", "UCore Backend Build 0%")
		backend.update_managed_backend(function(ok, update_message)
			if not ok then
				status.progress_fail("UCore Backend Build", "UCore Backend Build Failed")
				return callback(false, update_message)
			end

			status.progress_finish("UCore Backend Build", "UCore Backend Build 100%")
			replace_server()
		end)
	end)
end

-- Run setup and decide whether a full refresh is required.
-- 执行 setup，并判断是否需要 full refresh。
local function run_setup(payload, callback)
	log.write_progress("boot-setup", {
		project_root = payload.project_root,
	})
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
		log.write_progress("boot-engine-skip", {
			reason = "missing-engine-metadata",
		})
		finish_engine_skip_phases()
		return callback(true)
	end

	if not project.engine_needs_refresh(engine) then
		log.write_progress("boot-engine-skip", {
			reason = "shared-index-reused",
			engine_root = engine.engine_root,
		})
		finish_engine_skip_phases()
		return callback(true)
	end

	if engine_refreshing[engine.engine_id] then
		log.write_progress("boot-engine-skip", {
			reason = "refresh-already-running",
			engine_id = engine.engine_id,
		})
		finish_engine_skip_phases()
		return callback(true)
	end

	log.write_progress("boot-engine-start", {
		engine_id = engine.engine_id,
		engine_root = engine.engine_root,
	})
	engine_refreshing[engine.engine_id] = true
	local settled = false
	local title = "UCore Engine Discovery"

	local function finish_once(ok, err)
		if settled then
			return
		end
		settled = true
		engine_refreshing[engine.engine_id] = nil
		callback(ok, err)
	end

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
		if err then
			log.write_progress("boot-engine-finish", {
				ok = false,
				error = tostring(err),
			})
			return finish_once(false, err)
		end

		project.write_engine_index_metadata(engine)
		log.write_progress("boot-engine-finish", {
			ok = true,
			engine_id = engine.engine_id,
		})
		finish_once(true)
	end, {
		label = title,
		detail = "Scanning engine...",
	})
end

-- Refresh the shared Engine index as the final boot step.
-- 把共享 Engine 索引作为 boot 的最后一步顺序执行。
local function run_engine_refresh_step(payload, after_finish)
	run_engine_refresh_if_needed(payload, function(ok, err)
		if ok then
			finish_engine_status("UCore Engine Index Ready")
			if type(after_finish) == "function" then
				after_finish(true)
			end
			return
		end

		fail_engine_status("UCore Engine Discovery Failed: " .. tostring(err))
		if type(after_finish) == "function" then
			after_finish(false, err)
		end
	end)
end

-- Run refresh when setup says the database is stale or missing.
-- 当 setup 判断数据库缺失或过期时执行 refresh。
local function run_refresh_if_needed(payload, setup_result, callback)
	project_code_progress(0, "Checking project refresh...")

	if not setup_result.needs_full_refresh then
		log.write_progress("boot-project-skip", {
			reason = "index-up-to-date",
			project_root = payload.project_root,
		})
		finish_project_skip_phases()
		return callback(true)
	end

	log.write_progress("boot-project-refresh", {
		project_root = payload.project_root,
	})
	local refresh_payload = rust_payload(payload)
	refresh_payload.type = "refresh"
	refresh_payload.scope = "Game"

	client.refresh(refresh_payload, function(_, err)
		if err then
			return callback(false, err)
		end

		callback(true)
	end, {
		label = "UCore Project Discovery",
		detail = "Scanning project...",
	})
end

-- Start watcher after setup/refresh.
-- setup/refresh 后启动 watcher。
local function run_watch(payload, callback)
	log.write_progress("boot-watch", {
		project_root = payload.project_root,
	})
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
		return callback(false, "already booting")
	end

	local payload, err = current_project_payload(opts.project_root)
	if err then
		if err == "Could not find .uproject" then
			status.clear_all()
			return callback(false, err)
		end

		fail(err)
		return callback(false, err)
	end

	booting = true
	set_project_ready(payload.project_root, false)
	log.write_progress("boot-start", {
		project_root = payload.project_root,
	})
	status.start("UCore Initializing...")
	server_start_progress(0, "Starting backend...")
	server_ready_progress(0, "Waiting for RPC...")
	workspace_register_progress(0, "Registering workspace...")

	server.start(function(ok, start_message)
		if not ok then
			booting = false
			fail(start_message)
			return callback(false, start_message)
		end
		server_start_progress(100, "Backend started.")
		server_ready_progress(25, "Waiting for RPC...")

		wait_compatible(payload, false, function(ready, ready_err)
			if not ready then
				booting = false
				fail(tostring(ready_err), "Log: " .. tostring(server.log_path()))
				return callback(false, ready_err)
			end
			server_ready_progress(100, "RPC ready.")
			workspace_register_progress(40, "Registering workspace...")

			run_setup(payload, function(setup_ok, setup_result)
				if not setup_ok then
					booting = false
					fail(tostring(setup_result))
					return callback(false, setup_result)
				end
				workspace_register_progress(100, "Workspace registered.")

				-- After setup, fire refresh + watch in parallel and kick off
				-- engine refresh in the background. Project is marked ready as
				-- soon as refresh + watch finish — engine refresh no longer
				-- gates queries because engine searches fall back to SQLite.
				-- setup 完成后并行触发 refresh + watch，engine refresh 后台异步。
				-- 项目 ready 不再等 engine——engine 查询走 SQLite fallback。
				local refresh_done = false
				local refresh_failure = nil
				local watch_done = false
				local watch_failure = nil
				local finalized = false

				local function try_finalize()
					if finalized then
						return
					end
					if not (refresh_done and watch_done) then
						return
					end
					finalized = true

					local err_msg = refresh_failure or watch_failure
					if err_msg then
						booting = false
						fail(tostring(err_msg))
						return callback(false, err_msg)
					end

					project_finalize_progress(100)
					booting = false
					set_project_ready(payload.project_root, true)
					log.write_progress("boot-finish", {
						project_root = payload.project_root,
					})
					refresh_active_buffer_diagnostics()
					status.finish("UCore Ready - Project Indexed")
					callback(true)
				end

				run_refresh_if_needed(payload, setup_result, function(refresh_ok, refresh_err)
					refresh_done = true
					if not refresh_ok then
						refresh_failure = refresh_err
					end
					try_finalize()
				end)

				run_watch(payload, function(watch_ok, watch_err)
					watch_done = true
					if not watch_ok then
						watch_failure = watch_err
					end
					try_finalize()
				end)

				-- Fire-and-forget engine refresh. Status / log progress comes
				-- from inside run_engine_refresh_step. A failed engine refresh
				-- does NOT mark the project unready: engine queries fall back
				-- to SQLite FTS so the user keeps working.
				-- engine refresh 不阻塞 boot 完成，失败也不影响 project ready。
				run_engine_refresh_step(payload, function() end)
			end)
		end)
	end, {
		project_root = payload.project_root,
	})
end

function M.is_booting()
	return booting == true
end

function M.is_project_ready(project_root)
	return ready_projects[project_key(project_root)] == true
end

function M.mark_project_not_ready(project_root)
	set_project_ready(project_root, false)
end

function M.is_query_blocked(project_root)
	if booting then
		return true
	end

	if not project_root or project_root == "" then
		return false
	end

	return not M.is_project_ready(project_root)
end

return M
