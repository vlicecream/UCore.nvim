local bridge = require("ucore.bridge")
local install = require("ucore.install")
local project = require("ucore.project")

local M = {}

local uv = vim.uv or vim.loop
local session_ttl_seconds = 10

local function normalize(path)
	return path and path:gsub("\\", "/"):gsub("/+$", "") or nil
end

local function dirname(path)
	path = normalize(path or "")
	if path == "" then
		return nil
	end

	local parent = vim.fn.fnamemodify(path, ":h"):gsub("\\", "/")
	return normalize(parent)
end

local function mkdirp(path)
	path = normalize(path or "")
	if path == "" or vim.fn.isdirectory(path) == 1 then
		return true
	end

	vim.fn.mkdir(path, "p")
	return vim.fn.isdirectory(path) == 1
end

local function write_json(path, data)
	local parent = dirname(path)
	if parent then
		mkdirp(parent)
	end

	local file = assert(io.open(path, "wb"))
	file:write(vim.json.encode(data))
	file:close()
end

local function read_json(path)
	local file = path and io.open(path, "rb")
	if not file then
		return nil
	end

	local content = file:read("*a")
	file:close()
	local ok, decoded = pcall(vim.json.decode, content or "")
	if not ok or type(decoded) ~= "table" then
		return nil
	end

	return decoded
end

local function scandir_json(path)
	local items = {}
	local handle = uv.fs_scandir(path)
	if not handle then
		return items
	end

	while true do
		local name, kind = uv.fs_scandir_next(handle)
		if not name then
			break
		end
		if kind == "file" and name:sub(-5):lower() == ".json" then
			table.insert(items, normalize(path .. "/" .. name))
		end
	end

	table.sort(items)
	return items
end

local function base_dir()
	return dirname(bridge.registry_path())
end

local function request_dir()
	return normalize(base_dir() .. "/unreal-requests")
end

local function session_dir()
	return normalize(base_dir() .. "/unreal-sessions")
end

local function current_project_root()
	return normalize(project.find_project_root_from_context())
end

local function current_project_metadata(project_root)
	project_root = normalize(project_root or current_project_root())
	if not project_root then
		return nil, "Could not find .uproject"
	end

	local metadata = project.register_project(project_root)
	if not metadata or not metadata.uproject_path then
		return nil, "Could not find .uproject"
	end

	local engine, engine_err = project.engine_metadata(project_root)
	if not engine or not engine.engine_root then
		return nil, engine_err or "Could not resolve Unreal Engine root"
	end

	metadata.engine_root = engine.engine_root
	return metadata
end

local function session_alive(project_root)
	project_root = normalize(project_root)
	local now = os.time()

	for _, path in ipairs(scandir_json(session_dir())) do
		local item = read_json(path)
		if type(item) == "table" and normalize(item.project_root) == project_root then
			local last_seen = tonumber(item.last_seen or 0) or 0
			if now - last_seen <= session_ttl_seconds then
				return true
			end
		end
	end

	return false
end

local function editor_executable(engine_root)
	engine_root = normalize(engine_root)
	if not engine_root then
		return nil
	end

	local candidates = {
		engine_root .. "/Engine/Binaries/Win64/UnrealEditor.exe",
		engine_root .. "/Engine/Binaries/Win64/UnrealEditor-Win64-DebugGame.exe",
	}

	for _, path in ipairs(candidates) do
		if vim.fn.filereadable(path) == 1 then
			return normalize(path)
		end
	end

	return nil
end

local function launch_editor(metadata)
	local editor_path = editor_executable(metadata.engine_root)
	if not editor_path then
		return false, "Could not find UnrealEditor.exe"
	end

	local job = vim.fn.jobstart({ editor_path, metadata.uproject_path }, {
		detach = true,
	})
	if tonumber(job or 0) <= 0 then
		return false, "Failed to launch Unreal Editor"
	end

	return true
end

local function write_request(project_root, asset_path)
	local dir = request_dir()
	mkdirp(dir)

	local path = string.format(
		"%s/request-%d-%d.json",
		dir,
		vim.fn.getpid(),
		tonumber(uv.hrtime() % 1000000000)
	)

	write_json(path, {
		kind = "open_asset",
		project_root = normalize(project_root),
		asset_path = tostring(asset_path),
		timestamp = os.time(),
	})
end

function M.open(asset_path, opts)
	opts = opts or {}
	asset_path = tostring(asset_path or "")
	if asset_path == "" then
		return false, "Missing asset path"
	end

	local project_root = normalize(opts.project_root or current_project_root())
	if not project_root then
		return false, "Could not find .uproject"
	end

	local link_status = install.asset_link_status(project_root)
	if not link_status.ready then
		return false, "NeovimLink is not installed in this project or engine. Run :UCore install assetlink"
	end

	local metadata, err = current_project_metadata(project_root)
	if not metadata then
		return false, err
	end

	write_request(project_root, asset_path)

	if session_alive(project_root) then
		return true
	end

	return launch_editor(metadata)
end

function M.open_or_notify(asset_path, opts)
	local ok, err = M.open(asset_path, opts)
	if not ok then
		vim.notify("UCore asset open failed: " .. tostring(err), vim.log.levels.WARN)
	end
	return ok
end

return M
