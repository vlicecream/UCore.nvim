local config = require("ucore.config")

local M = {}
local read_json_file

local uv = vim.uv or vim.loop

local function is_windows()
	return package.config:sub(1, 1) == "\\"
end

-- Normalize paths to slash-separated strings for JSON/Rust interop.
-- 统一路径为斜杠格式，方便 JSON 和 Rust 侧处理。
local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function trim_trailing_slashes(path)
	path = normalize(path or "")
	if path == "" then
		return nil
	end
	if path == "/" or path:match("^%a:/$") then
		return path
	end
	path = path:gsub("/+$", "")
	return path ~= "" and path or nil
end

local function basename(path)
	path = normalize(path or "")
	path = path:gsub("/+$", "")
	return path:match("([^/]+)$") or ""
end

local function stable_hash12(text)
	text = tostring(text or "")
	local bitlib = bit or bit32
	local h1 = 2166136261
	local h2 = 16777619
	for i = 1, #text do
		local b = text:byte(i)
		h1 = bitlib.bxor(h1, b)
		h1 = (h1 * 16777619) % 4294967296
		h2 = bitlib.bxor(h2, b + i)
		h2 = (h2 * 2166136261) % 4294967296
	end
	return string.format("%08x%08x", h1, h2):sub(1, 12)
end

local function dirname(path)
	path = normalize(path or ""):gsub("/+$", "")
	return path:match("^(.*)/[^/]*$") or ""
end

local function canonicalize_path(path)
	path = tostring(path or "")
	path = vim.trim(path)
	if path == "" then
		return nil
	end
	path = vim.fn.expand(path)

	local absolute
	if vim.fs and vim.fs.abspath then
		absolute = vim.fs.abspath(path)
	else
		absolute = vim.fn.fnamemodify(path, ":p")
	end

	absolute = normalize(absolute)
	local native = is_windows() and absolute:gsub("/", "\\") or absolute
	local real = uv.fs_realpath(native) or uv.fs_realpath(absolute)
	return trim_trailing_slashes(real or absolute)
end

local function comparable_path(path)
	return canonicalize_path(path) or trim_trailing_slashes(path)
end

local function same_path(a, b)
	local left = comparable_path(a)
	local right = comparable_path(b)
	return left ~= nil and right ~= nil and left == right
end

local function fs_stat(path)
	return path and uv.fs_stat(path) or nil
end

local function is_file(path)
	local stat = fs_stat(path)
	return stat and stat.type == "file"
end

local function is_dir(path)
	local stat = fs_stat(path)
	return stat and stat.type == "directory"
end

local function mkdirp(path)
	path = normalize(path or "")
	if path == "" or is_dir(path) then
		return
	end

	local prefix = ""
	local rest = path
	local drive = rest:match("^%a:")
	if drive then
		prefix = drive
		rest = rest:sub(#drive + 1):gsub("^/+", "")
	elseif rest:sub(1, 1) == "/" then
		prefix = "/"
		rest = rest:gsub("^/+", "")
	end

	local current = prefix
	for part in rest:gmatch("[^/]+") do
		current = current == "" and part or (current:gsub("/$", "") .. "/" .. part)
		if not is_dir(current) then
			pcall(uv.fs_mkdir, current, 493)
		end
	end
end

local function normalize_association(association)
	association = tostring(association or "")
	association = vim.trim(association)
	if association == "" then
		return nil
	end
	return association
end

local function path_key(path)
	return canonicalize_path(path) or trim_trailing_slashes(path)
end

-- Return a readable project cache directory name.
-- 返回可读性较好的工程缓存目录名。
local function project_cache_name(project_root)
	local normalized = path_key(project_root) or normalize(project_root)
	local name = basename(normalized)
	local hash = stable_hash12(normalized)

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
	if is_dir(start_path) then
		dir = start_path
	else
		dir = dirname(normalize(vim.fs.abspath(start_path)))
	end

	local found = vim.fs.find(function(name)
		return name:match("%.uproject$")
	end, {
		path = dir,
		upward = true,
		type = "file",
		limit = 1,
	})[1]

	return found and (path_key(found) or normalize(found)) or nil
end

-- Return the Unreal project root directory.
-- 返回 Unreal 工程根目录。
function M.find_project_root(start_path)
	local project_file = M.find_project_file(start_path)
	if not project_file then
		return nil
	end

	return path_key(dirname(project_file)) or dirname(normalize(vim.fs.abspath(project_file)))
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
	local alt = tonumber(vim.v.alternate)
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
	project_root = path_key(project_root) or normalize(project_root)
	local cache_dir = normalize(config.values.cache_dir)
	local project_cache_dir = cache_dir .. "/projects/" .. project_cache_name(project_root)
	mkdirp(project_cache_dir)

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
	local engine_root = path_key(engine.engine_root) or normalize(engine.engine_root)
	local cache_dir = normalize(config.values.cache_dir)
	local registry = M.read_registry()
	local cache_engine_id = tostring(engine.engine_id or "")

	for registered_id, item in pairs(registry.engines or {}) do
		if type(item) == "table" then
			local registered_root = path_key(item.engine_root) or normalize(item.engine_root)
			if same_path(registered_root, engine_root) then
				cache_engine_id = tostring(item.engine_id or registered_id or cache_engine_id)
				break
			end
		end
	end

	local engine_cache_dir = cache_dir .. "/engines/" .. cache_engine_id
	mkdirp(engine_cache_dir)

	return {
		engine_id = cache_engine_id,
		engine_root = engine_root,
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
		engine_id = paths.engine_id,
		engine_root = paths.engine_root,
		engine_association = engine.engine_association,
		indexed_at = os.time(),
	}

	mkdirp(dirname(paths.metadata_path))
	local file = assert(io.open(paths.metadata_path, "wb"))
	file:write(vim.json.encode(metadata))
	file:close()

	local registry = M.read_registry()
	if paths.engine_id ~= engine.engine_id then
		registry.engines[engine.engine_id] = nil
	end
	registry.engines[paths.engine_id] = vim.tbl_deep_extend("force", registry.engines[paths.engine_id] or {}, metadata)
	M.write_registry(registry)

	return metadata
end

-- Return true when the shared Engine index is missing or stale.
-- 当共享 Engine 索引缺失或元信息不匹配时返回 true。
function M.engine_needs_refresh(engine)
	local paths = M.build_engine_paths(engine)

	if not is_file(paths.db_path) then
		return true
	end

	local metadata = M.read_engine_index_metadata(engine)
	if type(metadata) ~= "table" then
		return true
	end

	return not same_path(
		path_key(metadata.engine_root) or normalize(metadata.engine_root),
		path_key(engine.engine_root) or normalize(engine.engine_root)
	)
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
	mkdirp(cache_dir)
	return cache_dir .. "/registry.json"
end

-- Return the Rust server runtime registry path.
-- 返回 Rust server 运行态 registry 路径。
function M.server_registry_path()
	local cache_dir = normalize(config.values.cache_dir)
	mkdirp(cache_dir)
	return cache_dir .. "/server-registry.json"
end

-- Read the global project registry.
-- 读取全局项目注册表。
function M.read_registry()
	local path = M.global_registry_path()

	if not is_file(path) then
		return {
			projects = {},
			engines = {},
		}
	end

	local file = io.open(path, "rb")
	if not file then
		return {
			projects = {},
			engines = {},
		}
	end
	local content = file:read("*a")
	file:close()

	local ok_decode, data = pcall(vim.json.decode, content or "")
	if not ok_decode or type(data) ~= "table" then
		return {
			projects = {},
			engines = {},
		}
	end

	data.projects = data.projects or {}
	data.engines = data.engines or {}
	local dirty = false

	local normalized_projects = {}
	for root, item in pairs(data.projects) do
		if type(item) == "table" then
			local canonical_root = path_key(item.root or root) or normalize(item.root or root) or normalize(root)
			if canonical_root then
				item.root = canonical_root
				local existing_key = nil
				for candidate in pairs(normalized_projects) do
					if same_path(candidate, canonical_root) then
						existing_key = candidate
						break
					end
				end
				if existing_key then
					normalized_projects[existing_key] = vim.tbl_deep_extend("force", normalized_projects[existing_key], item)
					dirty = true
				else
					normalized_projects[canonical_root] = item
					if canonical_root ~= root then
						dirty = true
					end
				end
			else
				normalized_projects[root] = item
			end
		end
	end
	data.projects = normalized_projects

	local normalized_engines = {}
	for engine_id, item in pairs(data.engines) do
		if type(item) == "table" then
			local engine_root = path_key(item.engine_root) or normalize(item.engine_root)
			local association = normalize_association(item.engine_association)
			local canonical_id = (engine_root and M.engine_id(engine_root, association)) or tostring(item.engine_id or engine_id)
			if engine_root then
				item.engine_root = engine_root
			end
			item.engine_id = canonical_id
			if normalized_engines[canonical_id] then
				normalized_engines[canonical_id] = vim.tbl_deep_extend("force", normalized_engines[canonical_id], item)
				dirty = true
			else
				normalized_engines[canonical_id] = item
				if canonical_id ~= engine_id then
					dirty = true
				end
			end
		end
	end
	data.engines = normalized_engines

	if dirty then
		M.write_registry(data)
	end
	return data
end

-- Write the global project registry.
-- 写入全局项目注册表。
function M.write_registry(data)
	local path = M.global_registry_path()
	data.projects = data.projects or {}
	data.engines = data.engines or {}

	mkdirp(dirname(path))
	local file = assert(io.open(path, "wb"))
	file:write(vim.json.encode(data))
	file:close()
end

-- Find the .uproject file directly under a project root.
-- 在项目根目录下查找 .uproject 文件。
function M.find_project_file_in_root(project_root)
	project_root = path_key(project_root) or normalize(project_root)
	local scan = uv.fs_scandir(project_root)
	if not scan then
		return nil
	end
	while true do
		local name, t = uv.fs_scandir_next(scan)
		if not name then
			break
		end
		if t == "file" and name:match("%.uproject$") then
			return normalize(project_root .. "/" .. name)
		end
	end
	return nil
end

-- Read EngineAssociation from a .uproject file.
-- 从 .uproject 文件读取 EngineAssociation。
function M.read_engine_association(uproject_path)
	local file = uproject_path and io.open(uproject_path, "rb")
	if not file then
		return nil
	end
	local content = file:read("*a")
	file:close()

	local ok_decode, data = pcall(vim.json.decode, content or "")
	if not ok_decode or type(data) ~= "table" then
		return nil
	end

	return normalize_association(data.EngineAssociation)
end

-- Return display metadata for a project root.
-- 返回项目根目录对应的展示信息。
function M.project_metadata(project_root)
	project_root = path_key(project_root) or normalize(project_root)
	local uproject_path = M.find_project_file_in_root(project_root)
	local name = basename(project_root)
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
	project_root = path_key(project_root) or normalize(project_root)
	local registry = M.read_registry()
	local metadata = M.project_metadata(project_root)
	local existing_key = nil
	for root in pairs(registry.projects or {}) do
		if same_path(root, project_root) then
			existing_key = root
			break
		end
	end

	registry.projects[project_root] = vim.tbl_deep_extend("force", registry.projects[existing_key] or registry.projects[project_root] or {}, metadata)
	if existing_key and existing_key ~= project_root then
		registry.projects[existing_key] = nil
	end

	M.write_registry(registry)
	return metadata
end

-- Return registered projects as a sorted list.
-- 返回已注册项目的排序列表。
function M.list_registered_projects()
	local registry = M.read_registry()
	local items = {}
	local dirty = false

	for root, item in pairs(registry.projects or {}) do
		item.root = path_key(item.root or root) or item.root or root
		local association = normalize_association(item.engine_association)
		local engine_id = tostring(item.engine_id or "")
		if association == nil or engine_id == "" or engine_id:match("^%-") then
			local refreshed = M.project_metadata(item.root)
			item = vim.tbl_deep_extend("force", item, refreshed)
			registry.projects[root] = item
			dirty = true
		end
		table.insert(items, item)
	end

	if dirty then
		M.write_registry(registry)
	end

	table.sort(items, function(a, b)
		return tostring(a.name or a.root):lower() < tostring(b.name or b.root):lower()
	end)

	return items
end

-- Return cached engine metadata for a registered project.
-- 返回已注册项目缓存的 Engine 元信息。
function M.cached_engine_metadata(project_root)
	project_root = path_key(project_root) or normalize(project_root)

	local registry = M.read_registry()
	local item = registry.projects and registry.projects[project_root]
	if type(item) ~= "table" or not item.engine_id or not item.engine_root then
		for root, value in pairs(registry.projects or {}) do
			if same_path(root, project_root) then
				item = value
				break
			end
		end
	end
	if type(item) ~= "table" or not item.engine_id or not item.engine_root then
		return nil
	end

	return {
		engine_association = item.engine_association,
		engine_root = item.engine_root,
		engine_id = item.engine_id,
	}
end

local function find_loaded_file_buffer(path)
	path = path_key(path) or normalize(path)
	if not path then
		return nil
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) then
			local name = vim.api.nvim_buf_get_name(bufnr)
			if name ~= "" and same_path(name, path) then
				return bufnr
			end
		end
	end

	return nil
end

local function safe_open_project_target(target)
	target = path_key(target) or normalize(target)
	if not target then
		return
	end

	local existing = find_loaded_file_buffer(target)
	if existing then
		vim.api.nvim_set_current_buf(existing)
		return
	end

	local ok, err = pcall(vim.api.nvim_cmd, { cmd = "edit", args = { target } }, {})
	if ok then
		return
	end

	local message = tostring(err or "")
	if message:find("E325", 1, true) then
		vim.notify(
			"UCore: swap file exists for " .. vim.fn.fnamemodify(target, ":t") .. ". Workspace switched without opening it.",
			vim.log.levels.WARN
		)
		return
	end

	error(err)
end

-- Open a project by changing cwd and editing its .uproject when possible.
-- 通过切换 cwd 并打开 .uproject 来打开项目。
function M.open_project(project_root)
	project_root = path_key(project_root) or normalize(project_root)
	local metadata = M.register_project(project_root)

	vim.api.nvim_set_current_dir(project_root)

	if metadata.uproject_path then
		safe_open_project_target(metadata.uproject_path)
	else
		safe_open_project_target(project_root)
	end

	return metadata
end

-- Return true when a path looks like an Unreal Engine root.
-- 判断路径是否看起来像 Unreal Engine 根目录。
function M.is_engine_root(path)
	if not path or path == "" then
		return false
	end

	path = path_key(path) or normalize(path)
	return is_dir(path .. "/Engine/Source") or is_file(path .. "/Engine/Build/Build.version")
end

-- Read a JSON file safely.
-- 安全读取 JSON 文件。
function read_json_file(path)
	local file = path and io.open(path, "rb")
	if not file then
		return nil
	end
	local content = file:read("*a")
	file:close()

	local ok_decode, data = pcall(vim.json.decode, content or "")
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
			return path_key(root) or normalize(root)
		end
	end

	return nil
end

local function find_engine_association_from_config(engine_root)
	engine_root = path_key(engine_root) or normalize(engine_root)
	local roots = config.values.engine_roots or {}
	for key, root in pairs(roots) do
		if same_path(path_key(root) or normalize(root), engine_root) then
			return tostring(key)
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
			return path_key(install_location) or normalize(install_location)
		end
	end

	return nil
end

local function find_engine_association_from_launcher(engine_root)
	engine_root = path_key(engine_root) or normalize(engine_root)
	local data = read_json_file("C:/ProgramData/Epic/UnrealEngineLauncher/LauncherInstalled.dat")
	if type(data) ~= "table" or type(data.InstallationList) ~= "table" then
		return nil
	end

	for _, item in ipairs(data.InstallationList) do
		local app_name = item.AppName
		local install_location = path_key(item.InstallLocation) or normalize(item.InstallLocation)
		if app_name and same_path(install_location, engine_root) then
			return tostring(app_name)
		end
	end

	return nil
end

-- Find engine root from Unreal source-build registry entries.
-- 从 Unreal 源码版 registry entries 查找 engine root。
function M.find_engine_root_from_registry(association)
	if not is_windows() then
		return nil
	end

	local result = vim.system({
		"reg",
		"query",
		"HKCU\\Software\\Epic Games\\Unreal Engine\\Builds",
	}, { text = true }):wait()

	if result.code ~= 0 then
		return nil
	end

	local candidates = {}
	for _, key in ipairs(engine_association_candidates(association)) do
		candidates[key] = true
	end

	for line in (result.stdout or ""):gmatch("[^\r\n]+") do
		line = vim.trim(line)

		local name, path = line:match("^(%S+)%s+REG_SZ%s+(.+)$")
		path = path and vim.trim(path)

		if name and path and candidates[name] and M.is_engine_root(path) then
			return path_key(path) or normalize(path)
		end
	end

	return nil
end

local function find_engine_association_from_registry(engine_root)
	if not is_windows() then
		return nil
	end

	engine_root = path_key(engine_root) or normalize(engine_root)
	local result = vim.system({
		"reg",
		"query",
		"HKCU\\Software\\Epic Games\\Unreal Engine\\Builds",
	}, { text = true }):wait()

	if result.code ~= 0 then
		return nil
	end

	for line in (result.stdout or ""):gmatch("[^\r\n]+") do
		line = vim.trim(line)
		local name, path = line:match("^(%S+)%s+REG_SZ%s+(.+)$")
		path = path and (path_key(vim.trim(path)) or normalize(vim.trim(path)))
		if name and path and same_path(path, engine_root) then
			return tostring(name)
		end
	end

	return nil
end

local function resolve_engine_association_for_root(engine_root)
	engine_root = path_key(engine_root) or normalize(engine_root)
	if not engine_root or engine_root == "" then
		return nil
	end

	return normalize_association(find_engine_association_from_config(engine_root))
		or normalize_association(find_engine_association_from_launcher(engine_root))
		or normalize_association(find_engine_association_from_registry(engine_root))
end

-- Infer engine root by walking upward from a project nested inside a source tree.
-- 当项目内嵌在源码版引擎目录中时，通过向上查找推断引擎根目录。
function M.find_engine_root_from_project_path(project_root)
	project_root = path_key(project_root) or normalize(project_root)
	if not project_root or project_root == "" then
		return nil
	end

	local current = project_root
	while current and current ~= "" do
		if M.is_engine_root(current) then
			return current
		end

		local parent = dirname(current)
		if not parent or parent == "" or parent == current then
			break
		end
		current = parent
	end

	return nil
end

-- Resolve EngineAssociation to an Unreal Engine root path.
-- 将 EngineAssociation 解析成 Unreal Engine 根目录。
function M.resolve_engine_root(project_root)
	project_root = path_key(project_root) or normalize(project_root)

	local uproject_path = M.find_project_file_in_root(project_root)
	local association = normalize_association(M.read_engine_association(uproject_path))

	if not association or association == "" then
		local inferred_root = M.find_engine_root_from_project_path(project_root)
		if inferred_root then
			return inferred_root, resolve_engine_association_for_root(inferred_root)
		end
		return nil, "No EngineAssociation in .uproject"
	end

	if M.is_engine_root(association) then
		return path_key(association) or normalize(association), association
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

	engine_root = path_key(engine_root) or normalize(engine_root)
	local name = normalize_association(association) or basename(engine_root)
	local hash = stable_hash12(engine_root)
	return tostring(name):gsub("[^%w_.-]", "_") .. "-" .. hash
end

-- Resolve and return engine metadata for a project.
-- 解析并返回项目对应的 Engine 元信息。
function M.engine_metadata(project_root)
	local engine_root, association_or_err = M.resolve_engine_root(project_root)

	if not engine_root then
		return nil, association_or_err
	end

	local association = normalize_association(association_or_err)
		or resolve_engine_association_for_root(engine_root)
		or basename(engine_root)

	return {
		engine_association = association,
		engine_root = engine_root,
		engine_id = M.engine_id(engine_root, association),
	}
end

return M
