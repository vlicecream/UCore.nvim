local cli = require("ucore.client.cli")
local project = require("ucore.project")
local rpc = require("ucore.client.rpc")

local M = {}

-- Resolve the shared Engine DB for a project when it already exists.
-- 当共享 Engine DB 已存在时，解析当前项目对应的 Engine DB。
local function existing_engine_db_path(project_root)
	local engine = project.cached_engine_metadata(project_root) or project.engine_metadata(project_root)
	if not engine then
		return nil
	end

	local paths = project.build_engine_paths(engine)
	if vim.fn.filereadable(paths.db_path) ~= 1 then
		return nil
	end

	return paths.db_path
end

-- Send a typed query with the current project root attached.
-- 发送带 project_root 的类型化查询。
function M.query(project_root, query, callback)
	query.project_root = project_root
	query.engine_db_path = existing_engine_db_path(project_root)

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

-- Fetch completion candidates for the current buffer context.
-- 根据当前 buffer 上下文获取补全候选。
function M.get_completions(project_root, payload, callback)
	payload.kind = "GetCompletions"
	M.query(project_root, payload, callback)
end

-- Fetch go-to-definition target for the current buffer context.
-- 根据当前 buffer 上下文获取跳转定义目标。
function M.goto_definition(project_root, payload, callback)
	payload.kind = "GotoDefinition"
	M.query(project_root, payload, callback)
end

-- Find references/usages for one symbol.
-- 查找某个符号的引用/使用位置。
function M.find_references(project_root, payload, callback)
	payload.kind = "FindSymbolUsages"
	M.query(project_root, payload, callback)
end

-- Fetch indexed symbols declared in one source file.
-- 获取某个源码文件中声明的已索引符号。
function M.get_file_symbols(project_root, file_path, callback)
	M.query(project_root, {
		kind = "GetFileSymbols",
		file_path = file_path,
	}, callback)
end

-- Fetch members declared on a class.
-- 获取指定类声明的成员。
function M.get_class_members(project_root, class_name, callback)
	M.query(project_root, {
		kind = "GetClassMembers",
		class_name = class_name,
	}, callback)
end

-- Search symbols by text pattern.
-- 按文本模式搜索符号。
function M.search_symbols(project_root, pattern, callback, limit)
	M.query(project_root, {
		kind = "SearchSymbols",
		pattern = pattern,
		limit = limit or 50,
	}, callback)
end

return M
