local config = require("ucore.config")

local M = {}

-- Build a u_scanner CLI command from an RPC-like method and payload.
-- 根据类似 RPC 的 method 和 payload 构造 u_scanner CLI 命令。
local function build_cmd(method, payload)
	local cmd = vim.deepcopy(config.values.scanner_cmd)
	table.insert(cmd, method)

	if payload ~= nil then
		if type(payload) == "table" then
			table.insert(cmd, vim.json.encode(payload))
		else
			table.insert(cmd, payload)
		end
	end

	return cmd
end

-- Decode JSON stdout when possible; otherwise return raw text.
-- 优先把 stdout 当 JSON 解码，失败时返回原始文本。
local function decode_stdout(stdout)
	if not stdout or stdout == "" then
		return nil
	end

	local ok, decoded = pcall(vim.json.decode, stdout)
	if ok then
		return decoded
	end

	return stdout
end

-- Send one request through the u_scanner CLI bridge.
-- 通过 u_scanner CLI 桥发送一次请求。
function M.request(method, payload, callback)
	callback = callback or function() end

	local env = vim.tbl_extend("force", vim.fn.environ(), {
		UNL_SERVER_PORT = tostring(config.values.port),
	})

	vim.system(build_cmd(method, payload), {
		cwd = config.values.scanner_dir,
		text = true,
		env = env,
	}, function(result)
		vim.schedule(function()
			if result.code ~= 0 then
				return callback(nil, result.stderr ~= "" and result.stderr or result.stdout)
			end

			callback(decode_stdout(result.stdout), nil)
		end)
	end)
end

-- Query server status through the CLI bridge.
-- 通过 CLI 桥查询 server 状态。
function M.status(callback)
	M.request("status", nil, callback)
end

-- Register the current project in the server.
-- 在 server 中注册当前工程。
function M.setup(payload, callback)
	M.request("setup", payload, callback)
end

-- Rebuild or update the project index.
-- 重建或更新工程索引。
function M.refresh(payload, callback)
	M.request("refresh", payload, callback)
end

-- Start filesystem watching for the project.
-- 开始监听工程文件变化。
function M.watch(payload, callback)
	M.request("watch", payload, callback)
end

-- Run a query request through the CLI bridge.
-- 通过 CLI 桥发送查询请求。
function M.query(payload, callback)
	M.request("query", payload, callback)
end

return M
