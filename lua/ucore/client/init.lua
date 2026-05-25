local cli = require("ucore.client.cli")
local progress = require("ucore.progress")
local rpc = require("ucore.client.rpc")

local M = {
	cli = cli,
	rpc = rpc,
}

local function refresh_target_kind(payload, opts)
	if opts and opts.target_kind then
		return opts.target_kind
	end

	if payload and payload.engine_root == nil then
		return "engine"
	end

	return "project"
end

-- Keep most lifecycle commands on the CLI bridge, but run refresh over RPC so
-- progress notifications can reach Neovim.
-- 大多数生命周期命令仍走 CLI 桥；refresh 走 RPC，方便把进度通知送回 Neovim。
M.request = cli.request
M.status = cli.status
M.setup = cli.setup

-- Pick a user-facing progress title for refresh requests.
-- 为 refresh 请求选择面向用户的进度标题。
local function refresh_progress_title(payload, opts)
	if opts and opts.label then
		return opts.label
	end

	if refresh_target_kind(payload, opts) == "engine" then
		return "UCore Engine Discovery"
	end

	return "UCore Project Discovery"
end

local function refresh_progress_detail(payload, opts)
	if opts and opts.detail then
		return opts.detail
	end

	if payload and payload.engine_root == nil then
		return "Preparing engine refresh..."
	end

	return "Preparing project refresh..."
end

function M.refresh(payload, callback, opts)
	opts = vim.tbl_extend("force", {
		detail = refresh_progress_detail(payload, opts),
		target_kind = refresh_target_kind(payload, opts),
		auto_finish = true,
	}, opts or {})
	local show_progress = opts.silent ~= true
	local progress_msgid = nil
	local retry_attempts = tonumber(opts.rpc_retry_attempts or 20) or 20
	local retry_interval_ms = tonumber(opts.rpc_retry_interval_ms or 100) or 100
	local allow_cli_fallback = opts.allow_cli_fallback ~= false

	local function request_refresh(attempt)
		rpc.request("refresh", payload, function(result, err)
			if not err then
				if show_progress and opts.auto_finish ~= false then
					progress.finish(nil, progress_msgid)
				end
				return callback(result, nil)
			end

			local text = tostring(err)
			local is_conn_refused = text:find("ECONNREFUSED", 1, true) or text:lower():find("connection refused", 1, true)
			if is_conn_refused and attempt < retry_attempts then
				vim.defer_fn(function()
					request_refresh(attempt + 1)
				end, retry_interval_ms)
				return
			end

			if is_conn_refused and allow_cli_fallback then
				if show_progress and opts.auto_finish ~= false then
					progress.finish(nil, progress_msgid)
				end
				return cli.refresh(payload, callback)
			end

			if show_progress then
				progress.fail("UCore Index Failed: " .. text, progress_msgid)
			end
			callback(nil, err)
		end, {
			on_request = function(msgid)
				progress_msgid = msgid
				if show_progress then
					progress.start(refresh_progress_title(payload, opts), opts, msgid)
				end
			end,
		})
	end

	request_refresh(1)
end
M.watch = cli.watch
M.query = cli.query

return M
