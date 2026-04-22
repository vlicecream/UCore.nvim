local config = require("ucore.config")

local M = {}

-- Normalize paths to slash-separated strings for JSON/Rust interop.
-- 统一路径为斜杠格式，方便 JSON 和 Rust 侧处理。
local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

-- Return a readable project cache directory name.
-- 返回可读性较好的工程缓存目录名。
local function project_cache_name(project_root)
	local normalized = normalize(project_root)
	local name = vim.fn.fnamemodify(normalized, ":t")
	local hash = vim.fn.sha256(normalized):sub(1, 12)

	if name == "" then
		return hash
	end

	return name .. "-" .. hash
end

-- Find the nearest .uproject file by walking upward.
-- 从起始路径向上查找最近的 .uproject 文件。
function M.find_project_file(start_path)
	start_path = start_path or vim.api.nvim_buf_get_name(0)

	if start_path == "" then
		start_path = vim.loop.cwd()
	end

	local dir = vim.fn.fnamemodify(start_path, ":p:h")
	local found = vim.fs.find(function(name)
		return name:match("%.uproject$")
	end, {
		path = dir,
		upward = true,
		type = "file",
		limit = 1,
	})[1]

	return found and normalize(found) or nil
end

-- Return the Unreal project root directory.
-- 返回 Unreal 工程根目录。
function M.find_project_root(start_path)
	local project_file = M.find_project_file(start_path)
	if not project_file then
		return nil
	end

	return normalize(vim.fn.fnamemodify(project_file, ":p:h"))
end

-- Build default database paths under Neovim's cache directory.
-- 在 Neovim cache 目录下构造默认数据库路径。
function M.build_paths(project_root)
	local cache_dir = normalize(config.values.cache_dir)
	local project_cache_dir = cache_dir .. "/projects/" .. project_cache_name(project_root)
	vim.fn.mkdir(project_cache_dir, "p")

	return {
		project_root = project_root,
		cache_dir = project_cache_dir,
		db_path = project_cache_dir .. "/ucore.db",
		cache_db_path = project_cache_dir .. "/ucore-cache.db",
		registry_path = project_cache_dir .. "/registry.json",
		log_path = project_cache_dir .. "/u_core_server.log",
	}
end

-- Default scanner configuration shared by setup and refresh.
-- setup 和 refresh 共用的默认扫描配置。
function M.default_config()
	return {
		excludes_directory = {
			"Binaries",
			"Intermediate",
			"Saved",
			"Build",
			"DerivedDataCache",
			".git",
			".vs",
		},
		include_extensions = {
			"h",
			"hh",
			"hpp",
			"cpp",
			"cc",
			"cxx",
			"inl",
			"cs",
			"ini",
			"uasset",
			"umap",
			"uproject",
			"uplugin",
		},
	}
end

return M
