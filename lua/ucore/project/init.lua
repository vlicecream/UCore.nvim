local config = require("ucore.config")

local M = {}
local read_json_file

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

	if start_path == "" then
		return nil
	end

	local dir
	if vim.fn.isdirectory(start_path) == 1 then
		dir = start_path
	else
		dir = vim.fn.fnamemodify(start_path, ":p:h")
	end

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

-- Search for an Unreal project root from multiple context sources.
-- 从多个上下文来源搜索 Unreal 项目根目录。
function M.find_project_root_from_context(opts)
	opts = opts or {}

	-- 1. Current buffer file path
	local buf_path = vim.api.nvim_buf_get_name(0)
	if buf_path and buf_path ~= "" then
		local root = M.find_project_root(buf_path)
		if root then
			return root
		end
	end

	-- 2. Current working directory
	local cwd = vim.loop.cwd()
	if cwd then
		local root = M.find_project_root(cwd)
		if root then
			return root
		end
	end

	-- 3. Alternate buffer
	local alt = vim.fn.bufnr("#")
	if alt and alt > 0 then
		local alt_path = vim.api.nvim_buf_get_name(alt)
		if alt_path and alt_path ~= "" then
			local root = M.find_project_root(alt_path)
			if root then
				return root
			end
		end
	end

	-- 4. All listed normal file buffers
	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		local bo = vim.bo[bufnr]
		if bo.buflisted and bo.buftype == "" and bo.modifiable then
			local path = vim.api.nvim_buf_get_name(bufnr)
			if path and path ~= "" then
				local root = M.find_project_root(path)
				if root then
					return root
				end
			end
		end
	end

	-- 5. Registered projects fallback
	if opts.registered_fallback ~= false then
		local items = M.list_registered_projects()
		if not vim.tbl_isempty(items) then
			return items[1].root
		end
	end

	return nil
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
		project_registry_path = M.global_registry_path(),
		registry_path = M.server_registry_path(),
		log_path = project_cache_dir .. "/u_core_server.log",
	}
end

-- Build shared database paths for one Unreal Engine install.
-- 为一个 Unreal Engine 安装构造共享数据库路径。
function M.build_engine_paths(engine)
	local cache_dir = normalize(config.values.cache_dir)
	local engine_cache_dir = cache_dir .. "/engines/" .. engine.engine_id
	vim.fn.mkdir(engine_cache_dir, "p")

	return {
		engine_id = engine.engine_id,
		engine_root = engine.engine_root,
		cache_dir = engine_cache_dir,
		db_path = engine_cache_dir .. "/engine.db",
		cache_db_path = engine_cache_dir .. "/engine-cache.db",
		metadata_path = engine_cache_dir .. "/metadata.json",
	}
end

-- Read metadata for a shared Engine index.
-- 读取共享 Engine 索引的元信息。
function M.read_engine_index_metadata(engine)
	local paths = M.build_engine_paths(engine)
	return read_json_file(paths.metadata_path)
end

-- Write metadata for a shared Engine index.
-- 写入共享 Engine 索引的元信息。
function M.write_engine_index_metadata(engine)
	local paths = M.build_engine_paths(engine)
	local metadata = {
		engine_id = engine.engine_id,
		engine_root = engine.engine_root,
		engine_association = engine.engine_association,
		indexed_at = os.time(),
	}

	vim.fn.writefile(vim.split(vim.json.encode(metadata), "\n"), paths.metadata_path)

	local registry = M.read_registry()
	registry.engines[engine.engine_id] = vim.tbl_deep_extend("force", registry.engines[engine.engine_id] or {}, metadata)
	M.write_registry(registry)

	return metadata
end

-- Return true when the shared Engine index is missing or stale.
-- 当共享 Engine 索引缺失或元信息不匹配时返回 true。
function M.engine_needs_refresh(engine)
	local paths = M.build_engine_paths(engine)

	if vim.fn.filereadable(paths.db_path) ~= 1 then
		return true
	end

	local metadata = M.read_engine_index_metadata(engine)
	if type(metadata) ~= "table" then
		return true
	end

	return metadata.engine_id ~= engine.engine_id or normalize(metadata.engine_root) ~= normalize(engine.engine_root)
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

-- Return the global UCore registry path.
-- 返回 UCore 全局注册表路径。
function M.global_registry_path()
	local cache_dir = normalize(config.values.cache_dir)
	vim.fn.mkdir(cache_dir, "p")
	return cache_dir .. "/registry.json"
end

-- Return the Rust server runtime registry path.
-- 返回 Rust server 运行态 registry 路径。
function M.server_registry_path()
	local cache_dir = normalize(config.values.cache_dir)
	vim.fn.mkdir(cache_dir, "p")
	return cache_dir .. "/server-registry.json"
end

-- Read the global project registry.
-- 读取全局项目注册表。
function M.read_registry()
	local path = M.global_registry_path()

	if vim.fn.filereadable(path) ~= 1 then
		return {
			projects = {},
			engines = {},
		}
	end

	local ok, lines = pcall(vim.fn.readfile, path)
	if not ok then
		return {
			projects = {},
			engines = {},
		}
	end

	local ok_decode, data = pcall(vim.json.decode, table.concat(lines, "\n"))
	if not ok_decode or type(data) ~= "table" then
		return {
			projects = {},
			engines = {},
		}
	end

	data.projects = data.projects or {}
	data.engines = data.engines or {}
	return data
end

-- Write the global project registry.
-- 写入全局项目注册表。
function M.write_registry(data)
	local path = M.global_registry_path()
	data.projects = data.projects or {}
	data.engines = data.engines or {}

	vim.fn.mkdir(vim.fn.fnamemodify(path, ":p:h"), "p")
	vim.fn.writefile(vim.split(vim.json.encode(data), "\n"), path)
end

-- Find the .uproject file directly under a project root.
-- 在项目根目录下查找 .uproject 文件。
function M.find_project_file_in_root(project_root)
	local files = vim.fn.glob(project_root .. "/*.uproject", false, true)
	return files[1] and normalize(files[1]) or nil
end

-- Read EngineAssociation from a .uproject file.
-- 从 .uproject 文件读取 EngineAssociation。
function M.read_engine_association(uproject_path)
	if not uproject_path or vim.fn.filereadable(uproject_path) ~= 1 then
		return nil
	end

	local ok, lines = pcall(vim.fn.readfile, uproject_path)
	if not ok then
		return nil
	end

	local ok_decode, data = pcall(vim.json.decode, table.concat(lines, "\n"))
	if not ok_decode or type(data) ~= "table" then
		return nil
	end

	return data.EngineAssociation
end

-- Return display metadata for a project root.
-- 返回项目根目录对应的展示信息。
function M.project_metadata(project_root)
	project_root = normalize(project_root)
	local uproject_path = M.find_project_file_in_root(project_root)
	local name = vim.fn.fnamemodify(project_root, ":t")
	local engine = M.engine_metadata(project_root)

	return {
		name = name,
		root = project_root,
		uproject_path = uproject_path,
		engine_association = M.read_engine_association(uproject_path),
		engine_root = engine and engine.engine_root or nil,
		engine_id = engine and engine.engine_id or nil,
		registered_at = os.time(),
		last_opened_at = os.time(),
	}
end

-- Register one Unreal project into the global registry.
-- 将一个 Unreal 项目注册到全局注册表。
function M.register_project(project_root)
	project_root = normalize(project_root)
	local registry = M.read_registry()
	local metadata = M.project_metadata(project_root)

	registry.projects[project_root] = vim.tbl_deep_extend("force", registry.projects[project_root] or {}, metadata)

	M.write_registry(registry)
	return metadata
end

-- Return registered projects as a sorted list.
-- 返回已注册项目的排序列表。
function M.list_registered_projects()
	local registry = M.read_registry()
	local items = {}

	for root, item in pairs(registry.projects or {}) do
		item.root = item.root or root
		table.insert(items, item)
	end

	table.sort(items, function(a, b)
		return tostring(a.name or a.root):lower() < tostring(b.name or b.root):lower()
	end)

	return items
end

-- Return cached engine metadata for a registered project.
-- 返回已注册项目缓存的 Engine 元信息。
function M.cached_engine_metadata(project_root)
	project_root = normalize(project_root)

	local registry = M.read_registry()
	local item = registry.projects and registry.projects[project_root]
	if type(item) ~= "table" or not item.engine_id or not item.engine_root then
		return nil
	end

	return {
		engine_association = item.engine_association,
		engine_root = item.engine_root,
		engine_id = item.engine_id,
	}
end

-- Open a project by changing cwd and editing its .uproject when possible.
-- 通过切换 cwd 并打开 .uproject 来打开项目。
function M.open_project(project_root)
	project_root = normalize(project_root)
	local metadata = M.register_project(project_root)

	vim.cmd.cd(vim.fn.fnameescape(project_root))

	if metadata.uproject_path then
		vim.cmd.edit(vim.fn.fnameescape(metadata.uproject_path))
	else
		vim.cmd.edit(vim.fn.fnameescape(project_root))
	end

	return metadata
end

-- Return true when a path looks like an Unreal Engine root.
-- 判断路径是否看起来像 Unreal Engine 根目录。
function M.is_engine_root(path)
	if not path or path == "" then
		return false
	end

	path = normalize(path)
	return vim.fn.isdirectory(path .. "/Engine/Source") == 1
		or vim.fn.filereadable(path .. "/Engine/Build/Build.version") == 1
end

-- Read a JSON file safely.
-- 安全读取 JSON 文件。
function read_json_file(path)
	if vim.fn.filereadable(path) ~= 1 then
		return nil
	end

	local ok, lines = pcall(vim.fn.readfile, path)
	if not ok then
		return nil
	end

	local ok_decode, data = pcall(vim.json.decode, table.concat(lines, "\n"))
	if not ok_decode then
		return nil
	end

	return data
end

-- Normalize EngineAssociation variants for matching.
-- 规范化 EngineAssociation 的不同写法，方便匹配。
local function engine_association_candidates(association)
	if not association or association == "" then
		return {}
	end

	local items = { association }

	if not association:match("^UE_") then
		table.insert(items, "UE_" .. association)
	end

	if association:match("^UE_") then
		table.insert(items, association:gsub("^UE_", ""))
	end

	return items
end

-- Find engine root from user config.
-- 从用户配置中查找 engine root。
function M.find_engine_root_from_config(association)
	for _, key in ipairs(engine_association_candidates(association)) do
		local root = config.values.engine_roots and config.values.engine_roots[key]
		if M.is_engine_root(root) then
			return normalize(root)
		end
	end

	return nil
end

-- Find engine root from Epic LauncherInstalled.dat.
-- 从 Epic LauncherInstalled.dat 查找 engine root。
function M.find_engine_root_from_launcher(association)
	local data = read_json_file("C:/ProgramData/Epic/UnrealEngineLauncher/LauncherInstalled.dat")
	if type(data) ~= "table" or type(data.InstallationList) ~= "table" then
		return nil
	end

	local candidates = {}
	for _, key in ipairs(engine_association_candidates(association)) do
		candidates[key] = true
	end

	for _, item in ipairs(data.InstallationList) do
		local app_name = item.AppName
		local install_location = item.InstallLocation

		if candidates[app_name] and M.is_engine_root(install_location) then
			return normalize(install_location)
		end
	end

	return nil
end

-- Find engine root from Unreal source-build registry entries.
-- 从 Unreal 源码版 registry entries 查找 engine root。
function M.find_engine_root_from_registry(association)
	if vim.fn.has("win32") ~= 1 then
		return nil
	end

	local output = vim.fn.systemlist({
		"reg",
		"query",
		"HKCU\\Software\\Epic Games\\Unreal Engine\\Builds",
	})

	if vim.v.shell_error ~= 0 then
		return nil
	end

	local candidates = {}
	for _, key in ipairs(engine_association_candidates(association)) do
		candidates[key] = true
	end

	for _, line in ipairs(output) do
		line = vim.trim(line)

		local name, path = line:match("^(%S+)%s+REG_SZ%s+(.+)$")
		path = path and vim.trim(path)

		if name and path and candidates[name] and M.is_engine_root(path) then
			return normalize(path)
		end
	end

	return nil
end

-- Resolve EngineAssociation to an Unreal Engine root path.
-- 将 EngineAssociation 解析成 Unreal Engine 根目录。
function M.resolve_engine_root(project_root)
	project_root = normalize(project_root)

	local uproject_path = M.find_project_file_in_root(project_root)
	local association = M.read_engine_association(uproject_path)

	if not association or association == "" then
		return nil, "No EngineAssociation in .uproject"
	end

	if M.is_engine_root(association) then
		return normalize(association), association
	end

	local root = M.find_engine_root_from_config(association)
		or M.find_engine_root_from_launcher(association)
		or M.find_engine_root_from_registry(association)

	if root then
		return root, association
	end

	return nil, "Could not resolve Unreal Engine root for EngineAssociation: " .. tostring(association)
end

-- Build a stable engine id from root and association.
-- 根据 root 和 association 构造稳定 engine id。
function M.engine_id(engine_root, association)
	if not engine_root then
		return nil
	end

	local name = association or vim.fn.fnamemodify(engine_root, ":t")
	local hash = vim.fn.sha256(normalize(engine_root)):sub(1, 12)
	return tostring(name):gsub("[^%w_.-]", "_") .. "-" .. hash
end

-- Resolve and return engine metadata for a project.
-- 解析并返回项目对应的 Engine 元信息。
function M.engine_metadata(project_root)
	local engine_root, association_or_err = M.resolve_engine_root(project_root)

	if not engine_root then
		return nil, association_or_err
	end

	local uproject_path = M.find_project_file_in_root(project_root)
	local association = M.read_engine_association(uproject_path)

	return {
		engine_association = association,
		engine_root = engine_root,
		engine_id = M.engine_id(engine_root, association),
	}
end

return M
