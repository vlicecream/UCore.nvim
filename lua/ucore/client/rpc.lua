local config = require("ucore.config")
local log = require("ucore.log")
local progress = require("ucore.progress")

local uv = vim.uv or vim.loop
local M = {}

local HEARTBEAT_INTERVAL_MS = 120000

local socket = nil
local connected = false
local connecting = false
local read_buffer = ""
local next_msgid = 1
local pending = {}
local connect_waiters = {}
local heartbeat_timer = nil
local heartbeat_autocmd_registered = false

local function same_socket(handle)
	return handle ~= nil and socket ~= nil and handle == socket
end

local function stop_heartbeat()
	if heartbeat_timer then
		pcall(function()
			heartbeat_timer:stop()
		end)
		pcall(function()
			heartbeat_timer:close()
		end)
		heartbeat_timer = nil
	end
end

local function flush_connect_waiters(ok, err)
	local waiters = connect_waiters
	connect_waiters = {}
	connecting = false

	for _, callback in ipairs(waiters) do
		vim.schedule(function()
			callback(ok, err)
		end)
	end
end

local function send_heartbeat()
	M.request("ping", {
		pid = vim.fn.getpid(),
	}, function()
	end)
end

local function ensure_heartbeat()
	if heartbeat_timer and not heartbeat_timer:is_closing() then
		return
	end

	heartbeat_timer = uv.new_timer()
	if not heartbeat_timer then
		return
	end

	heartbeat_timer:start(
		HEARTBEAT_INTERVAL_MS,
		HEARTBEAT_INTERVAL_MS,
		vim.schedule_wrap(function()
			send_heartbeat()
		end)
	)

	if not heartbeat_autocmd_registered then
		heartbeat_autocmd_registered = true
		vim.schedule(function()
			vim.api.nvim_create_autocmd("VimLeavePre", {
				callback = function()
					stop_heartbeat()
				end,
			})
		end)
	end
end

-- Close the current socket without touching pending callbacks.
-- 关闭当前 socket，但不清理等待中的回调。
local function close_socket(handle)
	local target = handle or socket
	if target then
		pcall(function()
			target:read_stop()
		end)
		pcall(function()
			target:close()
		end)
	end

	if not handle or same_socket(handle) then
		socket = nil
		connected = false
		read_buffer = ""
	end
end

-- Encode a u32 as big-endian bytes.
-- 把 u32 编码成大端序字节。
local function u32_be(value)
	return string.char(
		math.floor(value / 16777216) % 256,
		math.floor(value / 65536) % 256,
		math.floor(value / 256) % 256,
		value % 256
	)
end

-- Decode a big-endian u32 from the first 4 bytes.
-- 从前 4 个字节解码大端序 u32。
local function read_u32_be(data)
	local b1, b2, b3, b4 = data:byte(1, 4)
	return ((b1 * 256 + b2) * 256 + b3) * 256 + b4
end

-- Wrap MessagePack payload with the Rust server frame header.
-- 给 MessagePack payload 加上 Rust server 使用的长度帧头。
local function make_frame(payload)
	return u32_be(#payload) .. payload
end

-- Try to extract one complete frame from the read buffer.
-- 尝试从读取缓冲区里取出一个完整帧。
local function take_frame()
	if #read_buffer < 4 then
		return nil
	end

	local len = read_u32_be(read_buffer)
	if #read_buffer < 4 + len then
		return nil
	end

	local frame = read_buffer:sub(5, 4 + len)
	read_buffer = read_buffer:sub(5 + len)
	return frame
end

-- Dispatch one decoded RPC frame.
-- 分发一个已解码的 RPC 帧。
local function handle_frame(frame)
	local ok, msg = pcall(vim.mpack.decode, frame)
	if not ok then
		vim.notify("UCore RPC decode failed: " .. tostring(msg), vim.log.levels.ERROR)
		return
	end

	local function decode_integer(value)
		local numeric = tonumber(value)
		if numeric ~= nil then
			return numeric
		end
		return value
	end

	local msg_type = decode_integer(msg[1])

	-- Response: [1, msgid, error, result]
	-- 响应帧：[1, msgid, error, result]
	if msg_type == 1 then
		local msgid = decode_integer(msg[2])
		local err = msg[3]
		local result = msg[4]
		local pending_entry = pending[msgid]
		pending[msgid] = nil
		local cb = pending_entry and pending_entry.callback

		if cb then
			vim.schedule(function()
				if err ~= nil and err ~= vim.NIL then
					cb(nil, err)
				else
					cb(result, nil)
				end
			end)
		end

		return
	end

	-- Notification: [2, method, params]
	-- 通知帧：[2, method, params]
	if msg_type == 2 then
		local method = msg[2]
		local params = msg[3]

		-- Capture the partial callback synchronously before scheduling. The
		-- response frame (msg_type == 1) clears `pending[msgid]` immediately,
		-- so if both partial and response land in the same TCP chunk and the
		-- partial lookup were done inside vim.schedule, pending would already
		-- be nil and the partial would be silently dropped.
		-- 在 schedule 之前同步取出 partial_callback。msg_type==1 会**同步**清掉
		-- pending[msgid]；如果 partial 和 response 落在同一个 chunk，等到
		-- schedule 执行时 pending 已被清空，partial 会被静默丢弃。
		local partial_msgid
		local partial_cb
		local partial_items
		local partial_append
		local partial_done
		if method == "query/partial" then
			partial_msgid = params and decode_integer(params.msgid)
			local pending_entry = partial_msgid and pending[partial_msgid] or nil
			partial_cb = pending_entry and pending_entry.partial_callback
			partial_items = params and params.items
			partial_append = params and params.append == true
			partial_done = params and params.done == true
		end

		vim.schedule(function()
		if method == "progress_plan" then
			local progress_msgid = type(params) == "table" and params.msgid or nil
			local progress_target_kind = type(params) == "table" and params.target_kind or nil
			local progress_payload = type(params) == "table" and (params.payload or params[2] or params) or params
			log.write_progress("rpc-progress-plan", {
				msgid = progress_msgid,
				target_kind = progress_target_kind,
				phase_count = type(progress_payload) == "table" and #(progress_payload.phases or progress_payload[2] or {}) or 0,
			})
			progress.handle_plan(progress_payload, progress_msgid, progress_target_kind)
			return
		end

		if method == "progress" then
			local progress_msgid = type(params) == "table" and params.msgid or nil
			local progress_target_kind = type(params) == "table" and params.target_kind or nil
			local progress_payload = type(params) == "table" and (params.payload or params[2] or params) or params
			log.write_progress("rpc-progress", {
				msgid = progress_msgid,
				target_kind = progress_target_kind,
				stage = progress_payload and (progress_payload.stage or progress_payload[2]) or nil,
				current = progress_payload and (progress_payload.current or progress_payload[3]) or nil,
				total = progress_payload and (progress_payload.total or progress_payload[4]) or nil,
				message = progress_payload and (progress_payload.message or progress_payload[5]) or nil,
			})
			progress.handle_progress(progress_payload, progress_msgid, progress_target_kind)
			return
		end

			if method == "query/partial" then
				if partial_cb then
					partial_cb(partial_items, nil, {
						append = partial_append,
						done = partial_done,
					})
				end
				return
			end
		end)
	end
end

-- Start reading frames from the TCP socket.
-- 开始从 TCP socket 读取响应帧。
local function start_read_loop(handle)
	if not handle then
		return
	end

	handle:read_start(function(err, chunk)
		if err then
			close_socket(handle)
			vim.schedule(function()
				vim.notify("UCore RPC read error: " .. tostring(err), vim.log.levels.ERROR)
			end)
			return
		end

		if not chunk then
			close_socket(handle)
			return
		end

		if not same_socket(handle) then
			return
		end

		read_buffer = read_buffer .. chunk

		while true do
			local frame = take_frame()
			if not frame then
				break
			end
			handle_frame(frame)
		end
	end)
end

-- Connect to the Rust server if needed.
-- 如果还没连接，则连接 Rust server。
function M.connect(callback)
	callback = callback or function() end

	if connected and socket then
		return callback(true, nil)
	end

	table.insert(connect_waiters, callback)
	if connecting then
		return
	end

	connecting = true
	local client = uv.new_tcp()
	if not client then
		close_socket()
		flush_connect_waiters(false, "UCore RPC socket could not be created")
		return
	end

	socket = client
	read_buffer = ""

	client:connect("127.0.0.1", config.values.port, function(err)
		if not same_socket(client) then
			return
		end

		if err then
			close_socket(client)
			return flush_connect_waiters(false, err)
		end

		connected = true
		start_read_loop(client)
		ensure_heartbeat()
		send_heartbeat()

		flush_connect_waiters(true, nil)
	end)
end

-- Send one RPC request over the persistent TCP connection.
-- 通过持久 TCP 连接发送一次 RPC 请求。
function M.request(method, params, callback, opts)
	callback = callback or function() end
	opts = opts or {}

	M.connect(function(ok, err)
		if not ok then
			return callback(nil, err)
		end

		local msgid = next_msgid
		next_msgid = next_msgid + 1
		pending[msgid] = {
			callback = callback,
			partial_callback = opts.partial_callback,
		}

		if type(opts.on_request) == "function" then
			pcall(opts.on_request, msgid)
		end

		-- Request: [0, msgid, method, params]
		-- 请求帧：[0, msgid, method, params]
		local payload = vim.mpack.encode({ 0, msgid, method, params or {} })
		local frame = make_frame(payload)
		local current_socket = socket
		if not current_socket then
			pending[msgid] = nil
			return callback(nil, "UCore RPC socket is not connected")
		end

		current_socket:write(frame, function(write_err)
			if write_err then
				pending[msgid] = nil
				if same_socket(current_socket) then
					close_socket(current_socket)
				end
				vim.schedule(function()
					callback(nil, write_err)
				end)
			end
		end)
	end)
end

-- Close the RPC socket and clear pending callbacks.
-- 关闭 RPC socket，并清理等待中的回调。
function M.close()
	close_socket()
	stop_heartbeat()
	pending = {}
	connect_waiters = {}
	connecting = false
end

return M
