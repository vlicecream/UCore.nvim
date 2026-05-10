local bootstrap = require("ucore.bootstrap")
local config = require("ucore.config")
local editor = require("ucore.editor")
local project = require("ucore.project")
local server = require("ucore.server")

local M = {}

local uv = vim.uv or vim.loop
local group_name = "UCoreBridge"

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function mkdirp(path)
	path = normalize(path or "")
	if path == "" or vim.fn.isdirectory(path) == 1 then
		return
	end

	vim.fn.mkdir(path, "p")
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

local function write_json(path, data)
	mkdirp(vim.fn.fnamemodify(path, ":h"):gsub("\\", "/"))
	local file = assert(io.open(path, "wb"))
	file:write(vim.json.encode(data))
	file:close()
end

local function default_bridge_dir()
	local explicit = vim.env.UE_UCORE_BRIDGE_DIR
	if explicit and explicit ~= "" then
		return normalize(explicit)
	end

	local localappdata = vim.env.LOCALAPPDATA
	if localappdata and localappdata ~= "" then
		return normalize(localappdata .. "/UnrealNVIM")
	end

	return normalize(config.values.cache_dir .. "/bridge")
end

function M.registry_path()
	local explicit = vim.env.UE_UCORE_BRIDGE_REGISTRY
	if explicit and explicit ~= "" then
		return normalize(explicit)
	end

	return normalize(default_bridge_dir() .. "/ucore-bridge.json")
end

local function server_dir()
	return normalize(default_bridge_dir() .. "/servers")
end

local function ensure_server()
	local current = tostring(vim.v.servername or "")
	if current ~= "" then
		vim.fn.setenv("UE_NVIM_SERVER", current)
		vim.fn.setenv("UE_UCORE_BRIDGE_REGISTRY", M.registry_path())
		return current
	end

	mkdirp(server_dir())
	local target = normalize(string.format("%s/ucore-%d.pipe", server_dir(), vim.fn.getpid()))
	local started = vim.fn.serverstart(target)
	local resolved = tostring(started or vim.v.servername or "")
	if resolved ~= "" then
		vim.fn.setenv("UE_NVIM_SERVER", resolved)
		vim.fn.setenv("UE_UCORE_BRIDGE_REGISTRY", M.registry_path())
	end
	return resolved
end

local function registry_template()
	return {
		version = 1,
		sessions = vim.empty_dict(),
		projects = vim.empty_dict(),
	}
end

local function prune_registry(data)
	data.sessions = type(data.sessions) == "table" and data.sessions or vim.empty_dict()
	data.projects = type(data.projects) == "table" and data.projects or vim.empty_dict()

	for project_root, item in pairs(data.projects) do
		if type(item) ~= "table" then
			data.projects[project_root] = nil
		else
			local server_name = tostring(item.server or "")
			if server_name == "" or type(data.sessions[server_name]) ~= "table" then
				data.projects[project_root] = nil
			end
		end
	end

	if next(data.sessions) == nil then
		data.sessions = vim.empty_dict()
	end
	if next(data.projects) == nil then
		data.projects = vim.empty_dict()
	end
end

local function load_registry()
	local data = read_json(M.registry_path()) or registry_template()
	prune_registry(data)
	return data
end

local function current_project_root()
	return project.find_project_root_from_context({ registered_fallback = false })
end

local function sync_registry(project_root)
	local server_name = ensure_server()
	if server_name == "" then
		return
	end

	local data = load_registry()
	local now = os.time()
	local cwd = normalize(uv.cwd() or vim.loop.cwd() or "")

	data.sessions[server_name] = {
		server = server_name,
		pid = vim.fn.getpid(),
		cwd = cwd,
		last_seen = now,
	}

	project_root = normalize(project_root)
	if project_root and project_root ~= "" then
		data.projects[project_root] = {
			server = server_name,
			project_root = project_root,
			cwd = cwd,
			pid = vim.fn.getpid(),
			last_seen = now,
		}
	end

	write_json(M.registry_path(), data)
end

local function ensure_project(project_root, opts)
	opts = opts or {}
	project_root = normalize(project_root)
	if not project_root or project_root == "" then
		return
	end

	project.register_project(project_root)

	local current_cwd = normalize(uv.cwd() or vim.loop.cwd() or "")
	if current_cwd ~= project_root then
		vim.api.nvim_set_current_dir(project_root)
	end

	sync_registry(project_root)

	if opts.boot == false then
		return
	end

	local paths = project.build_paths(project_root)
	if vim.fn.filereadable(paths.db_path) == 1 and server.is_running() then
		return
	end

	vim.schedule(function()
		bootstrap.boot(function() end, {
			project_root = project_root,
		})
	end)
end

local function open_request(payload)
	local files = payload.files
	if type(files) == "table" and not vim.tbl_isempty(files) then
		return editor.open_files(files, {
			line = payload.line or payload.line_number,
			col = payload.col or payload.column,
			silent = payload.silent,
		})
	end

	return editor.open_location(
		payload.file_path or payload.path,
		payload.line or payload.line_number,
		payload.col or payload.column,
		{ silent = payload.silent }
	)
end

local function dispatch(payload)
	if type(payload) ~= "table" then
		return false, "invalid request payload"
	end

	local kind = tostring(payload.kind or "open")
	local project_root = normalize(payload.project_root) or project.find_project_root(payload.file_path or payload.path or "")

	if kind == "activate_project" then
		ensure_project(project_root, { boot = payload.boot ~= false })
		return true, "ok"
	end

	if project_root and project_root ~= "" then
		ensure_project(project_root, { boot = payload.boot ~= false })
	end

	if kind == "open" or kind == "open_files" then
		if open_request(payload) then
			return true, "ok"
		end
		return false, "failed to open request target"
	end

	return false, "unknown bridge request: " .. kind
end

function M.handle_request(payload)
	local ok, result_or_err, maybe_message = pcall(dispatch, payload)
	if not ok then
		vim.schedule(function()
			vim.notify("UCore bridge failed: " .. tostring(result_or_err), vim.log.levels.ERROR)
		end)
		return "error"
	end

	if result_or_err then
		return maybe_message or "ok"
	end

	vim.schedule(function()
		vim.notify("UCore bridge failed: " .. tostring(maybe_message), vim.log.levels.WARN)
	end)
	return "error"
end

function M.handle_request_file(path)
	path = normalize(path)
	if not path or vim.fn.filereadable(path) ~= 1 then
		return "error"
	end

	local data = read_json(path)
	if not data then
		return "error"
	end

	pcall(vim.fn.delete, path)
	return M.handle_request(data)
end

local function clear_session()
	local server_name = tostring(vim.v.servername or "")
	if server_name == "" then
		return
	end

	local data = load_registry()
	data.sessions[server_name] = nil
	for root, item in pairs(data.projects or {}) do
		if type(item) == "table" and tostring(item.server or "") == server_name then
			data.projects[root] = nil
		end
	end
	if next(data.sessions) == nil then
		data.sessions = vim.empty_dict()
	end
	if next(data.projects) == nil then
		data.projects = vim.empty_dict()
	end
	write_json(M.registry_path(), data)
end

function M.setup()
	ensure_server()
	sync_registry(current_project_root())

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	vim.api.nvim_create_autocmd({ "VimEnter", "BufEnter", "DirChanged" }, {
		group = group,
		callback = function()
			sync_registry(current_project_root())
		end,
	})

	vim.api.nvim_create_autocmd("VimLeavePre", {
		group = group,
		callback = clear_session,
	})
end

function M.reset()
	pcall(vim.api.nvim_del_augroup_by_name, group_name)
	clear_session()
end

return M
