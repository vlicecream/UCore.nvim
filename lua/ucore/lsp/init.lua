local config = require("ucore.config")
local project = require("ucore.project")

local M = {}
local uv = vim.uv or vim.loop

local default_markers = {
	".clangd",
	".clang-tidy",
	".clang-format",
	"compile_commands.json",
	"compile_flags.txt",
	".git",
}

local default_filetypes = {
	"c",
	"cpp",
	"objc",
	"objcpp",
	"cuda",
	"proto",
	"unreal_cpp",
}

local default_clangd_args = {
	"--header-insertion=never",
	"--completion-style=detailed",
	"--function-arg-placeholders",
	"--pch-storage=disk",
	"--fallback-style=llvm",
}

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function path_join(...)
	return normalize(table.concat({ ... }, "/"):gsub("//+", "/"))
end

local function readable(path)
	return path and vim.fn.filereadable(path) == 1
end

local function executable(path)
	return path and (vim.fn.executable(path) == 1 or readable(path))
end

local function path_exists(path)
	return readable(path) or vim.fn.isdirectory(path) == 1
end

local function parent_dir(path)
	local normalized = normalize(path)
	if not normalized or normalized == "" then
		return nil
	end

	local parent = vim.fn.fnamemodify(normalized, ":h")
	if parent == normalized then
		return nil
	end

	return normalize(parent)
end

local function find_upward(start_path, markers)
	local current = start_path
	if not current or current == "" then
		current = vim.loop.cwd()
	end
	if not current or current == "" then
		return nil
	end

	current = normalize(current)
	if vim.fn.isdirectory(current) ~= 1 then
		current = normalize(vim.fn.fnamemodify(current, ":p:h"))
	end

	while current and current ~= "" do
		for _, marker in ipairs(markers) do
			if path_exists(path_join(current, marker)) then
				return current
			end
		end

		local parent = parent_dir(current)
		if not parent or parent == current then
			break
		end
		current = parent
	end

	return nil
end

local function deep_copy(value)
	return vim.deepcopy(value)
end

local function configured_lsp()
	return (config.values.lsp and config.values.lsp.clangd) or {}
end

local function configured_auto_setup()
	local lsp = config.values.lsp or {}
	return lsp.auto_setup ~= false
end

local function has_native_lsp_enable()
	return type(vim.lsp) == "table" and type(vim.lsp.config) ~= "nil" and type(vim.lsp.enable) == "function"
end

local function refresh_native_lsp_buffers()
	if not has_native_lsp_enable() then
		return
	end

	vim.schedule(function()
		pcall(vim.cmd.doautoall, "nvim.lsp.enable FileType")
	end)
end

local function windows_clangd_candidates()
	local candidates = {
		"C:/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/Llvm/x64/bin/clangd.exe",
		"C:/Program Files/LLVM/bin/clangd.exe",
	}

	local patterns = {
		"C:/Program Files/Microsoft Visual Studio/*/*/VC/Tools/Llvm/x64/bin/clangd.exe",
		"C:/Program Files/Microsoft Visual Studio/*/*/VC/Tools/Llvm/bin/clangd.exe",
	}

	for _, pattern in ipairs(patterns) do
		for _, path in ipairs(vim.fn.glob(pattern, false, true)) do
			table.insert(candidates, normalize(path))
		end
	end

	return candidates
end

local function is_windows_vs_llvm_bin_clangd(path)
	path = normalize(path)
	if not path then
		return false
	end

	path = path:lower()
	return path:match("/vc/tools/llvm/bin/clangd%.exe$") ~= nil
end

local function normalize_compile_db_dir(value)
	value = normalize(value)
	if not value or value == "" then
		return nil
	end

	if value:match("compile_commands%.json$") then
		return normalize(vim.fn.fnamemodify(value, ":p:h"))
	end

	return value
end

local function has_compile_commands(dir)
	dir = normalize_compile_db_dir(dir)
	return dir and path_exists(path_join(dir, "compile_commands.json")) and dir or nil
end

local function project_cache_compile_commands_dir(root)
	root = normalize(root)
	if not root or root == "" then
		return nil
	end

	local paths = project.build_paths(root)
	local clangd_dir = path_join(paths.cache_dir, "clangd")
	vim.fn.mkdir(clangd_dir, "p")
	return normalize(clangd_dir)
end

local function project_compile_commands_dir(root)
	root = normalize(root)
	if not root or root == "" then
		return nil
	end

	local cache_dir = project_cache_compile_commands_dir(root)
	local cached = has_compile_commands(cache_dir)
	if cached then
		return cached
	end

	return has_compile_commands(root)
end

local function copy_file(source_path, target_path)
	if uv and uv.fs_copyfile then
		local ok_copy = pcall(uv.fs_copyfile, source_path, target_path)
		if ok_copy or readable(target_path) then
			return true
		end
	else
		local ok_read, lines = pcall(vim.fn.readfile, source_path)
		if not ok_read then
			return false
		end

		local ok_write = pcall(vim.fn.writefile, lines, target_path)
		if ok_write then
			return true
		end
	end

	return false
end

local function delete_file(path)
	if not path or path == "" then
		return false
	end

	if uv and uv.fs_unlink then
		local ok = pcall(uv.fs_unlink, path)
		return ok
	end

	return pcall(vim.fn.delete, path) and vim.fn.filereadable(path) == 0
end

local function project_name(root)
	local project_file = project.find_project_file_in_root(root)
	if not project_file then
		return vim.fn.fnamemodify(root, ":t")
	end

	return vim.fn.fnamemodify(project_file, ":t:r")
end

local function editor_target_name(root)
	local base_name = project_name(root)
	local preferred = base_name .. "Editor"
	local candidates = vim.fn.glob(path_join(root, "Source/*.Target.cs"), false, true)
	local fallback = nil

	for _, path in ipairs(candidates) do
		local name = tostring(path):match("([^/\\]+)%.Target%.cs$")
		if name then
			if name == preferred then
				return preferred
			end
			if name:match("Editor$") and not fallback then
				fallback = name
			end
		end
	end

	return fallback or preferred
end

local function unreal_build_tool(root)
	local engine = project.cached_engine_metadata(root) or project.engine_metadata(root)
	if not engine or not engine.engine_root then
		return nil, "failed to resolve Unreal Engine root"
	end

	local candidates = {
		path_join(engine.engine_root, "Engine/Binaries/DotNET/UnrealBuildTool/UnrealBuildTool.exe"),
		path_join(engine.engine_root, "Engine/Binaries/DotNET/UnrealBuildTool/UnrealBuildTool.dll"),
	}

	for _, path in ipairs(candidates) do
		if executable(path) then
			return path, nil, engine
		end
	end

	return nil, "UnrealBuildTool not found under: " .. tostring(engine.engine_root), engine
end

local function build_base_capabilities()
	return {
		textDocument = {
			completion = {
				editsNearCursor = true,
			},
		},
		offsetEncoding = { "utf-8", "utf-16" },
	}
end

function M.find_root(fname)
	if type(fname) == "number" then
		fname = vim.api.nvim_buf_get_name(fname)
	end

	local unreal_root = project.find_project_root(fname)
	if unreal_root then
		return unreal_root
	end

	return find_upward(fname, default_markers)
end

function M.resolve_clangd_command()
	local clangd = configured_lsp()
	local configured = clangd.command or "clangd"

	if vim.fn.has("win32") == 1 and clangd.auto_detect_windows ~= false then
		if configured == "clangd" or configured == "clangd.exe" or is_windows_vs_llvm_bin_clangd(configured) then
			for _, candidate in ipairs(windows_clangd_candidates()) do
				if executable(candidate) then
					return normalize(candidate)
				end
			end
		end
	end

	if executable(configured) then
		return normalize(configured)
	end

	if configured ~= "clangd" and configured ~= "clangd.exe" then
		return configured
	end

	return configured
end

function M.should_attach(root)
	root = normalize(root)
	if not root or root == "" then
		return false
	end

	local clangd = configured_lsp()
	if clangd.require_compile_commands == false then
		return true
	end

	return M.find_compilation_database(root) ~= nil
end

function M.find_compilation_database(root)
	root = normalize(root)
	if not root or root == "" then
		return nil
	end

	local project_db = project_compile_commands_dir(root)
	if project_db then
		return project_db
	end

	local clangd = configured_lsp()
	local configured = clangd.compile_commands_dir

	if type(configured) == "function" then
		configured = configured(root)
	end

	configured = has_compile_commands(configured)
	if configured then
		return configured
	end

	local candidates = {
		root,
		path_join(root, ".vscode"),
		path_join(root, "build"),
		path_join(root, "Build"),
		path_join(root, "Intermediate"),
		path_join(root, "Intermediate/Build"),
		path_join(root, ".cache"),
	}

	local engine = project.cached_engine_metadata(root) or project.engine_metadata(root)
	if engine and engine.engine_root then
		table.insert(candidates, normalize(engine.engine_root))
		table.insert(candidates, path_join(engine.engine_root, "Engine"))
	end

	for _, dir in ipairs(candidates) do
		local found = has_compile_commands(dir)
		if found then
			return found
		end
	end

	return nil
end

function M.ensure_project_compile_database(root, opts)
	opts = opts or {}
	root = normalize(root)
	if not root or root == "" then
		return nil
	end

	local cache_dir = project_cache_compile_commands_dir(root)
	if not cache_dir then
		return nil
	end

	local cached = has_compile_commands(cache_dir)
	local root_dir_db = has_compile_commands(root)
	if cached and not opts.refresh then
		if opts.remove_source == true and root_dir_db and normalize(root_dir_db) == root then
			delete_file(path_join(root, "compile_commands.json"))
		end
		return cached
	end

	local source_dir = (opts.refresh and root_dir_db and root) or M.find_compilation_database(root)
	if not source_dir then
		return nil
	end

	local source_path = path_join(source_dir, "compile_commands.json")
	local target_path = path_join(cache_dir, "compile_commands.json")
	if not readable(source_path) then
		return nil
	end

	if normalize(source_path) ~= normalize(target_path) and not copy_file(source_path, target_path) then
		return nil
	end

	if opts.remove_source == true and source_dir == root and normalize(source_path) ~= normalize(target_path) then
		delete_file(source_path)
	end

	return cache_dir
end

function M.clangd_status(root)
	root = normalize(root or M.find_root(vim.api.nvim_buf_get_name(0)))
	local clangd = configured_lsp()
	local command = M.resolve_clangd_command()
	local compile_commands_dir = root and M.find_compilation_database(root) or nil
	local ubt, ubt_err, engine = nil, nil, nil
	if root then
		ubt, ubt_err, engine = unreal_build_tool(root)
	end

	return {
		auto_setup = configured_auto_setup(),
		command = command,
		command_available = executable(command),
		require_compile_commands = clangd.require_compile_commands ~= false,
		project_root = root,
		compile_commands_dir = compile_commands_dir,
		can_attach = root ~= nil and M.should_attach(root),
		target = root and editor_target_name(root) or nil,
		unreal_build_tool = ubt,
		unreal_build_tool_error = ubt_err,
		engine_root = engine and engine.engine_root or nil,
	}
end

function M.generate_compile_commands(root, opts, callback)
	opts = opts or {}
	callback = callback or function() end
	root = normalize(root or project.find_project_root())
	if not root or root == "" then
		return callback(false, "Could not find .uproject")
	end

	local uproject = project.find_project_file_in_root(root)
	if not uproject then
		return callback(false, "Could not find .uproject under: " .. root)
	end

	local ubt, ubt_err = unreal_build_tool(root)
	if not ubt then
		return callback(false, ubt_err or "UnrealBuildTool not found")
	end

	local target = opts.target or editor_target_name(root)
	local platform = opts.platform or "Win64"
	local configuration = opts.configuration or "Development"

	local cmd
	if ubt:match("%.dll$") then
		cmd = {
			"dotnet",
			ubt,
			"-Mode=GenerateClangDatabase",
			target,
			platform,
			configuration,
			"-Project=" .. uproject,
		}
	else
		cmd = {
			ubt,
			"-Mode=GenerateClangDatabase",
			target,
			platform,
			configuration,
			"-Project=" .. uproject,
		}
	end

	vim.system(cmd, {
		cwd = root,
		text = true,
	}, function(result)
		local ok = result.code == 0
		if ok then
			M.ensure_project_compile_database(root, { refresh = true, remove_source = true })
			refresh_native_lsp_buffers()
		end
		local compile_commands_dir = M.find_compilation_database(root)
		local payload = {
			code = result.code,
			cmd = cmd,
			stdout = result.stdout,
			stderr = result.stderr,
			compile_commands_dir = compile_commands_dir,
			target = target,
			platform = platform,
			configuration = configuration,
		}

		if ok and compile_commands_dir then
			return callback(true, payload)
		end

		if ok then
			return callback(false, "GenerateClangDatabase succeeded but compile_commands.json was not found")
		end

		local output = vim.trim(table.concat({
			result.stdout or "",
			result.stderr or "",
		}, "\n"))

		callback(false, output ~= "" and output or ("GenerateClangDatabase failed with exit code " .. tostring(result.code)))
	end)
end

function M.get_capabilities(capabilities)
	local merged = vim.tbl_deep_extend("force", build_base_capabilities(), capabilities or {})
	local clangd = configured_lsp()

	if clangd.prefer_blink_capabilities == false then
		return merged
	end

	local ok, blink = pcall(require, "blink.cmp")
	if ok and blink and type(blink.get_lsp_capabilities) == "function" then
		return blink.get_lsp_capabilities(merged)
	end

	return merged
end

function M.clangd_config(opts)
	opts = opts or {}
	local clangd = configured_lsp()
	local command = M.resolve_clangd_command()
	local cmd = opts.cmd or clangd.cmd

	if not cmd then
		cmd = { command }
		for _, arg in ipairs(clangd.args or default_clangd_args) do
			table.insert(cmd, arg)
		end
	end

	local user_filetypes = opts.filetypes or clangd.filetypes
	local filetypes = {}
	for _, ft in ipairs(user_filetypes or default_filetypes) do
		table.insert(filetypes, ft)
	end
	if not vim.tbl_contains(filetypes, "unreal_cpp") then
		table.insert(filetypes, "unreal_cpp")
	end

	local root_dir = opts.root_dir
	if type(root_dir) ~= "function" then
		if has_native_lsp_enable() then
			root_dir = function(bufnr, on_dir)
				local root = M.find_root(bufnr)
				if not M.should_attach(root) then
					return
				end

				M.ensure_project_compile_database(root)
				on_dir(root)
			end
		else
			root_dir = function(fname)
				local root = M.find_root(fname)
				if not M.should_attach(root) then
					return nil
				end

				M.ensure_project_compile_database(root)
				return root
			end
		end
	end

	local capabilities = M.get_capabilities(opts.capabilities or clangd.capabilities)
	local user_on_new_config = opts.on_new_config or clangd.on_new_config

	local merged = vim.tbl_deep_extend("force", {
		cmd = cmd,
		filetypes = filetypes,
		root_dir = root_dir,
		single_file_support = opts.single_file_support
			or clangd.single_file_support
			or false,
		capabilities = capabilities,
		init_options = vim.tbl_deep_extend("force", {
			clangdFileStatus = true,
		}, deep_copy(clangd.init_options or {})),
	}, opts)

	merged.on_new_config = function(new_config, new_root_dir)
		local has_compile_dir = false
		for _, arg in ipairs(new_config.cmd or {}) do
			if tostring(arg):find("^%-%-compile%-commands%-dir=", 1) then
				has_compile_dir = true
				break
			end
		end

		if not has_compile_dir then
			local compile_commands_dir = M.find_compilation_database(new_root_dir)
			if compile_commands_dir then
				table.insert(new_config.cmd, "--compile-commands-dir=" .. normalize(compile_commands_dir))
			end
		end

		if type(user_on_new_config) == "function" then
			user_on_new_config(new_config, new_root_dir)
		end
	end

	return merged
end

function M.setup_clangd(opts)
	local cfg = M.clangd_config(opts)

	if has_native_lsp_enable() then
		local current_root = project.find_project_root_from_context()
		if current_root then
			M.ensure_project_compile_database(current_root)
		end

		local ok = pcall(function()
			vim.lsp.config("clangd", cfg)
			vim.lsp.enable("clangd")
		end)
		if ok then
			refresh_native_lsp_buffers()
			return cfg
		end
	end

	local ok, lspconfig = pcall(require, "lspconfig")
	if not ok then
		error("nvim-lspconfig is required for ucore.lsp.setup_clangd()")
	end

	lspconfig.clangd.setup(cfg)
	return cfg
end

return M
