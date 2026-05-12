local cli = require("ucore.client.cli")
local bootstrap = require("ucore.bootstrap")
local completion_debug = require("ucore.completion.debug")
local project = require("ucore.project")
local rpc = require("ucore.client.rpc")

local M = {}

local function blocked_query_result(kind)
	if kind == "GetDiagnostics" or kind == "ParseBuildDiagnostics" then
		return { items = {} }
	end

	if kind == "GetCompletions" then
		return { items = {} }
	end

	if kind == "GetSignatureHelp" then
		return { signatures = {} }
	end

	return {}
end

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
	if bootstrap.is_booting() then
		return callback(blocked_query_result(query and query.kind), nil)
	end

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
		cli.query(query, function(cli_result, cli_err)
			callback(cli_result, cli_err)
		end)
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

-- Fetch resolved Unreal config values.
-- 获取解析后的 Unreal 配置数据。
function M.get_config_data(project_root, callback)
	local engine = project.cached_engine_metadata(project_root) or project.engine_metadata(project_root)

	M.query(project_root, {
		kind = "GetConfigData",
		engine_root = engine and engine.engine_root or nil,
	}, callback)
end

-- Fetch completion candidates for the current buffer context.
-- 根据当前 buffer 上下文获取补全候选。
function M.get_completions(project_root, payload, callback)
	payload.kind = "GetCompletions"
	completion_debug.log(
		"remote",
		"send",
		string.format("line=%s", tostring(payload.line)),
		string.format("char=%s", tostring(payload.character)),
		tostring(payload.file_path or "")
	)
	M.query(project_root, payload, function(result, err)
		if err then
			completion_debug.log("remote", "error", tostring(err))
			return callback(result, err)
		end

		local count = 0
		if type(result) == "table" then
			local items = result.items or result.completions or result
			if type(items) == "table" then
				count = #items
			end
		end
		completion_debug.log("remote", "result", string.format("items=%s", count))
		callback(result, err)
	end)
end

-- Fetch UCore diagnostics for the current buffer.
-- 获取当前 buffer 的 UCore 诊断。
function M.get_diagnostics(project_root, payload, callback)
	payload.kind = "GetDiagnostics"
	M.query(project_root, payload, callback)
end

-- Parse build output into diagnostics.
-- 将构建输出解析为诊断。
function M.parse_build_diagnostics(project_root, output, callback)
	M.query(project_root, {
		kind = "ParseBuildDiagnostics",
		output = output,
	}, callback)
end

-- Fetch go-to-definition target for the current buffer context.
-- 根据当前 buffer 上下文获取跳转定义目标。
function M.goto_definition(project_root, payload, callback)
	payload.kind = "GotoDefinition"
	M.query(project_root, payload, callback)
end

-- Fetch go-to-implementation target (.h -> .cpp).
-- 获取跳转实现目标（.h -> .cpp）。
function M.goto_implementation(project_root, payload, callback)
	payload.kind = "GotoImplementation"
	M.query(project_root, payload, callback)
end

-- Fetch hover information for the current buffer context.
-- 根据当前 buffer 上下文获取 hover 信息。
function M.get_hover(project_root, payload, callback)
	payload.kind = "GetHover"
	M.query(project_root, payload, callback)
end

-- Fetch signature help for the call expression around cursor.
-- 获取当前光标所在调用表达式的签名帮助。
function M.get_signature_help(project_root, payload, callback)
	payload.kind = "GetSignatureHelp"
	M.query(project_root, payload, callback)
end

-- Find references/usages for one symbol.
-- 查找某个符号的引用/使用位置。
function M.find_references(project_root, payload, callback)
	payload.kind = "FindSymbolUsages"
	M.query(project_root, payload, callback)
end

-- Parse one in-memory buffer and return cursor metadata.
-- 解析内存中的当前 buffer，并返回光标相关元数据。
function M.parse_buffer(project_root, payload, callback)
	payload.kind = "ParseBuffer"
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
function M.search_symbols(project_root, pattern, callback, opts)
	if type(opts) == "number" then
		opts = { limit = opts }
	end
	opts = opts or {}

	M.query(project_root, {
		kind = "SearchSymbols",
		pattern = pattern,
		limit = opts.limit or 50,
		offset = opts.offset or 0,
	}, callback)
end

function M.search_class_symbols(project_root, pattern, callback, opts)
	if type(opts) == "number" then
		opts = { limit = opts }
	end
	opts = opts or {}

	M.query(project_root, {
		kind = "SearchClassSymbols",
		pattern = pattern,
		limit = opts.limit or 50,
		offset = opts.offset or 0,
	}, callback)
end

function M.find_derived_classes(project_root, base_class, callback)
	M.query(project_root, {
		kind = "FindDerivedClasses",
		base_class = base_class,
	}, callback)
end

function M.get_asset_usages(project_root, asset_path, callback)
	M.query(project_root, {
		kind = "GetAssetUsages",
		asset_path = asset_path,
	}, callback)
end

function M.get_asset_dependencies(project_root, asset_path, callback)
	M.query(project_root, {
		kind = "GetAssetDependencies",
		asset_path = asset_path,
	}, callback)
end

function M.fast_find(project_root, pattern, callback, opts)
	if type(opts) == "number" then
		opts = { limit = opts }
	end
	opts = opts or {}

	M.query(project_root, {
		kind = "FastFind",
		pattern = pattern or "",
		limit = opts.limit or 50,
		offset = opts.offset or 0,
		scope = opts.scope,
	}, callback)
end

function M.search_code_text(project_root, pattern, callback, opts)
	if type(opts) == "number" then
		opts = { limit = opts }
	end
	opts = opts or {}

	M.query(project_root, {
		kind = "SearchCodeText",
		pattern = pattern or "",
		limit = opts.limit or 50,
		offset = opts.offset or 0,
		scope = opts.scope or "project",
	}, callback)
end

-- Unified global find: symbols, files, and code text.
-- 统一全局搜索：symbol、文件名/路径和代码文本。
function M.global_find(project_root, pattern, callback, opts)
	if type(opts) == "number" then
		opts = { limit = opts }
	end
	opts = opts or {}

	M.query(project_root, {
		kind = "GlobalFind",
		pattern = pattern or "",
		limit = opts.limit or 50,
		offset = opts.offset or 0,
	}, callback)
end

-- Search indexed files by filename or path part.
-- 按文件名或路径片段搜索已索引文件。
function M.search_files(project_root, pattern, callback)
	M.query(project_root, {
		kind = "SearchFilesByPathPart",
		part = pattern or "",
	}, callback)
end

return M
