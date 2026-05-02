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

local function restart_clangd_clients(root)
	root = normalize(root)
	if not root or root == "" then
		return false
	end

	local restarted = false
	for _, client in ipairs(vim.lsp.get_clients({ name = "clangd" })) do
		if normalize(client.root_dir) == root then
			restarted = true
			vim.lsp.stop_client(client.id)
		end
	end

	return restarted
end

local function refresh_project_buffers(root)
	root = normalize(root)
	if not root or root == "" then
		return
	end

	vim.schedule(function()
		local restarted = restart_clangd_clients(root)

		local function reattach()
			refresh_native_lsp_buffers()

			local current = vim.api.nvim_get_current_buf()
			if not vim.api.nvim_buf_is_valid(current) then
				return
			end

			local current_path = normalize(vim.api.nvim_buf_get_name(current))
			if not current_path or current_path == "" then
				return
			end

			if project.find_project_root(current_path) ~= root then
				return
			end

			if vim.bo[current].modified or vim.bo[current].buftype ~= "" then
				return
			end

			pcall(vim.cmd, "silent edit")
		end

		if restarted then
			vim.defer_fn(reattach, 200)
		else
			reattach()
		end
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

local function should_filter_clangd_diagnostic(diagnostic, clangd)
	if clangd.suppress_unused_include_warnings == false then
		return false
	end

	if type(diagnostic) ~= "table" then
		return false
	end

	local source = tostring(diagnostic.source or ""):lower()
	if source ~= "" and source ~= "clangd" then
		return false
	end

	local code = diagnostic.code
	if type(code) == "table" then
		code = code.value
	end
	code = tostring(code or ""):lower()

	local message = tostring(diagnostic.message or ""):lower()

	if code:find("unused%-includes", 1, false) then
		return true
	end

	if message:find("unused include", 1, true) then
		return true
	end

	if message:find("included header", 1, true) and message:find("not used", 1, true) then
		return true
	end

	if message:find("not used directly", 1, true) and message:find("include", 1, true) then
		return true
	end

	return false
end

local function clangd_diagnostic_code(diagnostic)
	local code = diagnostic and diagnostic.code
	if type(code) == "table" then
		code = code.value
	end
	return tostring(code or ""):lower()
end

local function clangd_diagnostic_message(diagnostic)
	return tostring(diagnostic and diagnostic.message or ""):lower()
end

local function should_expand_clangd_range(diagnostic)
	local code = clangd_diagnostic_code(diagnostic)
	local message = clangd_diagnostic_message(diagnostic)

	if code:find("unused", 1, true) then
		return true
	end

	if code == "c4189" then
		return true
	end

	return message:find("unused variable", 1, true) ~= nil
		or message:find("unused parameter", 1, true) ~= nil
		or message:find("unused field", 1, true) ~= nil
		or message:find("private field", 1, true) ~= nil and message:find("not used", 1, true) ~= nil
end

local function expand_clangd_diagnostic_range(bufnr, diagnostic)
	if not should_expand_clangd_range(diagnostic) then
		return diagnostic
	end

	local filetype = vim.bo[bufnr] and vim.bo[bufnr].filetype or ""
	if filetype ~= "unreal_cpp" and filetype ~= "cpp" and filetype ~= "c" then
		return diagnostic
	end

	local ok_parser, parser = pcall(vim.treesitter.get_parser, bufnr, filetype)
	if not ok_parser or not parser then
		return diagnostic
	end

	local trees = parser:parse()
	local tree = trees and trees[1]
	if not tree then
		return diagnostic
	end

	local range = diagnostic.range or {}
	local start_range = range.start or {}
	local finish_range = range["end"] or start_range
	local start_row = tonumber(start_range.line)
	local start_col = tonumber(start_range.character)
	local end_row = tonumber(finish_range.line or start_row)
	local end_col = tonumber(finish_range.character or start_col)
	if not start_row or not start_col then
		return diagnostic
	end

	local root = tree:root()
	local node = root:descendant_for_range(start_row, start_col, end_row, math.max(end_col, start_col + 1))
	if not node then
		return diagnostic
	end

	local preferred = {
		init_declarator = true,
		parameter_declaration = true,
		field_declaration = true,
		declaration = true,
	}

	while node do
		local kind = node:type()
		if preferred[kind] then
			local srow, scol, erow, ecol = node:range()
			local expanded = vim.deepcopy(diagnostic)
			expanded.range = {
				start = { line = srow, character = scol },
				["end"] = { line = erow, character = ecol },
			}
			return expanded
		end
		node = node:parent()
	end

	return diagnostic
end

local function filtered_publish_diagnostics_handler(user_handler)
	local base = user_handler or vim.lsp.handlers["textDocument/publishDiagnostics"]

	return function(err, result, ctx, cfg)
		local clangd = configured_lsp()
		if result and type(result.diagnostics) == "table" then
			local bufnr = ctx and ctx.bufnr or nil
			if not bufnr and result.uri then
				local ok_uri, resolved = pcall(vim.uri_to_bufnr, result.uri)
				if ok_uri then
					bufnr = resolved
				end
			end

			local filtered = {}
			for _, diagnostic in ipairs(result.diagnostics) do
				if not should_filter_clangd_diagnostic(diagnostic, clangd) then
					table.insert(
						filtered,
						bufnr and vim.api.nvim_buf_is_valid(bufnr)
							and expand_clangd_diagnostic_range(bufnr, diagnostic)
							or diagnostic
					)
				end
			end
			result = vim.tbl_extend("force", result, { diagnostics = filtered })
		end

		return base(err, result, ctx, cfg)
	end
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
		auto_generate_compile_commands = clangd.auto_generate_compile_commands ~= false,
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
			refresh_project_buffers(root)
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

function M.prepare_compile_commands(root, opts, callback)
	opts = opts or {}
	callback = callback or function() end
	root = normalize(root or project.find_project_root())
	if not root or root == "" then
		return callback(false, "Could not find .uproject")
	end

	local ready_dir = M.ensure_project_compile_database(root, {
		remove_source = opts.remove_source ~= false,
	})
	if ready_dir then
		refresh_project_buffers(root)
		return callback(true, {
			compile_commands_dir = ready_dir,
			generated = false,
			staged = true,
		})
	end

	local clangd = configured_lsp()
	if opts.auto_generate == false or clangd.auto_generate_compile_commands == false then
		return callback(false, "compile_commands.json not found")
	end

	M.generate_compile_commands(root, opts, callback)
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
		if has_native_lsp_enable() then
			cmd = function(dispatchers, client_config)
				local root = normalize(client_config.root_dir)
				local resolved = { command }
				for _, arg in ipairs(clangd.args or default_clangd_args) do
					table.insert(resolved, arg)
				end

				local compile_commands_dir = root and M.find_compilation_database(root) or nil
				if compile_commands_dir then
					table.insert(resolved, "--compile-commands-dir=" .. normalize(compile_commands_dir))
				end

				return vim.lsp.rpc.start(resolved, dispatchers, {
					cwd = root or client_config.cmd_cwd,
					env = client_config.cmd_env,
					detached = client_config.detached,
				})
			end
		else
			cmd = { command }
			for _, arg in ipairs(clangd.args or default_clangd_args) do
				table.insert(cmd, arg)
			end
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
	local user_handlers = deep_copy(opts.handlers or clangd.handlers or {})
	user_handlers["textDocument/publishDiagnostics"] = filtered_publish_diagnostics_handler(
		user_handlers["textDocument/publishDiagnostics"]
	)

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
		handlers = user_handlers,
	}, opts)

	merged.on_new_config = function(new_config, new_root_dir)
		local has_compile_dir = false
		if type(new_config.cmd) == "table" then
			for _, arg in ipairs(new_config.cmd) do
				if tostring(arg):find("^%-%-compile%-commands%-dir=", 1) then
					has_compile_dir = true
					break
				end
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
