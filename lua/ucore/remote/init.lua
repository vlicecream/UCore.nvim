local cli = require("ucore.client.cli")
local rpc = require("ucore.client.rpc")

local M = {}

-- Send a typed query with the current project root attached.
-- 发送带 project_root 的类型化查询。
function M.query(project_root, query, callback)
	query.project_root = project_root

	-- Prefer the persistent TCP RPC path for interactive queries.
	-- 交互式查询优先走持久 TCP RPC，避免每次启动 CLI 进程。
	rpc.request("query", query, function(result, err)
		if not err then
			return callback(result, nil)
		end

		-- Fall back to the CLI bridge so early development stays forgiving.
		-- RPC 失败时回退到 CLI 桥，方便开发阶段排查问题。
		cli.query(query, callback)
	end)
end

-- Fetch project components such as Game, Engine, and plugins.
-- 获取工程组件，例如 Game、Engine、插件等。
function M.get_components(project_root, callback)
	M.query(project_root, {
		kind = "GetComponents",
	}, callback)
end

-- Fetch indexed Unreal modules from the Rust database.
-- 从 Rust 数据库获取已索引的 Unreal 模块。
function M.get_modules(project_root, callback)
	M.query(project_root, {
		kind = "GetModules",
	}, callback)
end

-- Fetch indexed asset graph entries from the Rust server.
-- 从 Rust server 获取已索引的资产图条目。
function M.get_assets(project_root, callback)
	M.query(project_root, {
		kind = "GetAssets",
	}, callback)
end

-- Search symbols by text pattern.
-- 按文本模式搜索符号。
function M.search_symbols(project_root, pattern, callback)
	M.query(project_root, {
		kind = "SearchSymbols",
		pattern = pattern,
		limit = 50,
	}, callback)
end

return M
