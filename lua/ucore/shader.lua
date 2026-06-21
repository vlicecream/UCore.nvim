local config = require("ucore.config")

local M = {}
local group_name = "UCoreShader"
local client_name = "hlsl_lsp"
local uv = vim.uv or vim.loop

local shader_extensions = {
	hlsl = true,
	hlsli = true,
	usf = true,
	ush = true,
}

-- Normalize one filesystem path to absolute slash-separated form.
-- 将文件系统路径规范化为绝对正斜杠形式。
local function normalize(path)
	if not path or path == "" then
		return nil
	end

	return vim.fn.fnamemodify(path, ":p"):gsub("\\", "/")
end

-- Return whether the path exists as a file or directory.
-- 返回路径是否以文件或目录形式存在。
local function path_exists(path)
	path = normalize(path)
	return path and uv.fs_stat(path) ~= nil or false
end

-- Return whether the path exists as a directory.
-- 返回路径是否以目录形式存在。
local function is_dir(path)
	path = normalize(path)
	local stat = path and uv.fs_stat(path) or nil
	return stat and stat.type == "directory" or false
end

-- Return the configured shader feature block.
-- 返回配置中的 shader 功能块。
local function shader_config()
	return config.values.shader or {}
end

-- Return the configured shader LSP block.
-- 返回配置中的 shader LSP 配置块。
local function shader_lsp_config()
	return shader_config().lsp or {}
end

-- Return whether shader support is enabled.
-- 返回是否启用了 shader 支持。
local function shader_enabled()
	return shader_config().enable ~= false
end

-- Return whether shader LSP support is enabled.
-- 返回是否启用了 shader LSP 支持。
local function shader_lsp_enabled()
	return shader_enabled() and shader_lsp_config().enable ~= false
end

-- Return whether the path points to a supported shader source file.
-- 返回路径是否指向受支持的 shader 源文件。
local function is_shader_path(path)
	path = normalize(path)
	local ext = path and path:match("%.([^.\\/]*)$")
	return ext and shader_extensions[ext:lower()] == true or false
end

-- Return the extension parent directories searched for HLSL tools.
-- 返回搜索 HLSL 工具时使用的扩展父目录列表。
local function extension_parent_dirs()
	local dirs = {}
	local home = normalize(vim.fn.expand("$HOME"))
	if home then
		dirs[#dirs + 1] = normalize(home .. "/.vscode/extensions")
		dirs[#dirs + 1] = normalize(home .. "/.cursor/extensions")
	end

	for _, item in ipairs(shader_lsp_config().extension_dirs or {}) do
		local path = normalize(item)
		if path then
			dirs[#dirs + 1] = path
		end
	end

	return dirs
end

-- Return the matching HLSL extension directories under one parent directory.
-- 返回一个父目录下匹配的 HLSL 扩展目录列表。
local function matching_extension_dirs(base)
	local dirs = {}
	base = normalize(base)
	if not base or not is_dir(base) then
		return dirs
	end

	local handle = uv.fs_scandir(base)
	if not handle then
		return dirs
	end

	while true do
		local name, kind = uv.fs_scandir_next(handle)
		if not name then
			break
		end

		local lower = name:lower()
		if kind == "directory"
			and (lower:find("hlsl", 1, true) or lower:find("shader", 1, true))
		then
			dirs[#dirs + 1] = normalize(base .. "/" .. name)
		end
	end

	return dirs
end

-- Find the newest installed HLSL-related VS Code extension directory.
-- 查找最新安装的 HLSL 相关 VS Code 扩展目录。
local function find_installed_extension()
	local matches = {}
	for _, base in ipairs(extension_parent_dirs()) do
		for _, dir in ipairs(matching_extension_dirs(base)) do
			matches[#matches + 1] = dir
		end
	end

	table.sort(matches, function(left, right)
		return tostring(left) > tostring(right)
	end)

	return matches[1]
end

-- Return candidate executable paths for common HLSL language services.
-- 返回常见 HLSL 语言服务的候选可执行路径。
local function lsp_binary_candidates(ext_dir)
	local candidates = {}
	ext_dir = normalize(ext_dir)
	if not ext_dir then
		return candidates
	end

	for _, rel in ipairs({
		"/bin/server.exe",
		"/bin/server",
		"/server/bin/server.exe",
		"/server/bin/server",
		"/out/server.js",
		"/dist/server.js",
		"/language-server/out/server.js",
		"/LanguageServer/bin/Debug/net8.0/ShaderTools.LanguageServer.dll",
		"/LanguageServer/bin/Release/net8.0/ShaderTools.LanguageServer.dll",
	}) do
		candidates[#candidates + 1] = normalize(ext_dir .. rel)
	end

	return candidates
end

-- Find the HLSL LSP command from config overrides or installed extensions.
-- 从配置覆盖或已安装扩展中查找 HLSL LSP 命令。
local function lsp_cmd()
	local cmd = shader_lsp_config().cmd
	if type(cmd) == "table" and #cmd > 0 then
		return vim.deepcopy(cmd)
	end
	if type(cmd) == "string" and cmd ~= "" then
		return { cmd }
	end

	local ext_dir = find_installed_extension()
	if not ext_dir then
		return nil
	end

	for _, candidate in ipairs(lsp_binary_candidates(ext_dir)) do
		if path_exists(candidate) then
			local lower = candidate:lower()
			if lower:match("%.js$") then
				return { "node", candidate }
			end
			if lower:match("%.dll$") then
				return { "dotnet", candidate }
			end
			return { candidate }
		end
	end

	return nil
end

-- Return one command line string used for health/status display.
-- 返回用于 health/status 显示的一条命令行字符串。
local function lsp_cmd_label()
	local cmd = lsp_cmd()
	if not cmd or #cmd == 0 then
		return "<not found>"
	end
	return table.concat(cmd, " ")
end

-- Find the shader project root by walking upward from the given path.
-- 从给定路径向上查找 shader 项目根目录。
function M.find_project_root(start_path)
	local path = normalize(start_path)
	if not path or path == "" then
		return nil
	end

	local dir = is_dir(path) and path or vim.fs.dirname(path)
	if not dir then
		return nil
	end

	local markers = vim.fs.find(function(name)
		return name:match("%.uproject$")
			or name:match("%.uplugin$")
			or name == ".git"
	end, {
		path = dir,
		upward = true,
		type = "file",
		limit = 1,
	})

	if markers[1] then
		return normalize(vim.fs.dirname(markers[1]))
	end

	return normalize(dir)
end

-- Apply shader filetype detection to one buffer when it matches known extensions.
-- 在缓冲区匹配已知扩展时应用 shader 文件类型检测。
local function apply_buffer_filetype(bufnr)
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local path = vim.api.nvim_buf_get_name(bufnr)
	if path == "" or not is_shader_path(path) then
		return
	end

	if vim.bo[bufnr].filetype ~= "hlsl" then
		vim.api.nvim_buf_call(bufnr, function()
			vim.cmd("setfiletype hlsl")
		end)
	end
end

-- Return whether an HLSL LSP client is already attached to the buffer.
-- 返回 HLSL LSP 客户端是否已经附加到该缓冲区。
local function has_attached_client(bufnr)
	if type(vim.lsp.get_clients) ~= "function" then
		return false
	end

	for _, client in ipairs(vim.lsp.get_clients({ bufnr = bufnr }) or {}) do
		if client.name == client_name then
			return true
		end
	end

	return false
end

-- Apply buffer-local defaults that make LSP completion available immediately.
-- 应用 buffer 局部默认值，使 LSP 补全可立即使用。
local function apply_buffer_defaults(bufnr)
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	if vim.bo[bufnr].omnifunc == "" then
		vim.bo[bufnr].omnifunc = "v:lua.vim.lsp.omnifunc"
	end
end

-- Start the HLSL LSP client for one buffer when the environment is ready.
-- 在环境就绪时为单个缓冲区启动 HLSL LSP 客户端。
local function start_lsp(bufnr)
	if not shader_lsp_enabled() or type(vim.lsp.start) ~= "function" then
		return
	end
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) or vim.bo[bufnr].filetype ~= "hlsl" then
		return
	end
	if has_attached_client(bufnr) then
		apply_buffer_defaults(bufnr)
		return
	end

	local cmd = lsp_cmd()
	if not cmd or #cmd == 0 then
		return
	end

	local path = vim.api.nvim_buf_get_name(bufnr)
	local root = M.find_project_root(path)
	if not root then
		return
	end

	vim.lsp.start({
		name = client_name,
		cmd = cmd,
		root_dir = root,
	}, {
		bufnr = bufnr,
	})

	apply_buffer_defaults(bufnr)
end

-- Open hover information through the active shader LSP client.
-- 通过当前 shader LSP 客户端打开 hover 信息。
function M.hover()
	if vim.bo.filetype ~= "hlsl" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.hover()
end

-- Open signature help through the active shader LSP client.
-- 通过当前 shader LSP 客户端打开签名帮助。
function M.signature_help()
	if vim.bo.filetype ~= "hlsl" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.signature_help()
end

-- Jump to the definition through the active shader LSP client.
-- 通过当前 shader LSP 客户端跳转到定义。
function M.definition()
	if vim.bo.filetype ~= "hlsl" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.definition()
end

-- Find references through the active shader LSP client.
-- 通过当前 shader LSP 客户端查找引用。
function M.references()
	if vim.bo.filetype ~= "hlsl" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.references()
end

-- Rename the symbol through the active shader LSP client.
-- 通过当前 shader LSP 客户端重命名符号。
function M.rename()
	if vim.bo.filetype ~= "hlsl" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.rename()
end

-- Stop active HLSL LSP clients attached to the current Neovim instance.
-- 停止当前 Neovim 实例里激活的 HLSL LSP 客户端。
function M.stop_lsp()
	if type(vim.lsp.get_clients) ~= "function" then
		return 0
	end

	local stopped = 0
	for _, client in ipairs(vim.lsp.get_clients({ name = client_name }) or {}) do
		if client and type(client.stop) == "function" then
			client:stop()
			stopped = stopped + 1
		end
	end

	return stopped
end

-- Restart the HLSL LSP client for the current buffer when possible.
-- 在可行时为当前缓冲区重启 HLSL LSP 客户端。
function M.restart_lsp()
	local stopped = M.stop_lsp()
	start_lsp(vim.api.nvim_get_current_buf())
	return stopped
end

-- Register filetype detection and LSP startup hooks for shader buffers.
-- 为 shader 缓冲区注册文件类型检测和 LSP 启动钩子。
function M.setup()
	if not shader_enabled() then
		return
	end

	vim.filetype.add({
		extension = {
			hlsl = "hlsl",
			hlsli = "hlsl",
			usf = "hlsl",
			ush = "hlsl",
		},
	})

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufEnter" }, {
		group = group,
		pattern = { "*.hlsl", "*.hlsli", "*.usf", "*.ush" },
		callback = function(ev)
			vim.schedule(function()
				apply_buffer_filetype(ev.buf)
			end)
		end,
	})

	vim.api.nvim_create_autocmd("FileType", {
		group = group,
		pattern = "hlsl",
		callback = function(ev)
			apply_buffer_defaults(ev.buf)
			if shader_lsp_config().auto_start ~= false then
				vim.schedule(function()
					start_lsp(ev.buf)
				end)
			end
		end,
	})
end

-- Reset shader-specific autocmds.
-- 重置 shader 相关的自动命令。
function M.reset()
	pcall(vim.api.nvim_del_augroup_by_name, group_name)
end

-- Return a summary of the current shader integration state.
-- 返回当前 shader 集成状态摘要。
function M.info()
	return {
		"shader enabled: " .. tostring(shader_enabled()),
		"shader lsp enabled: " .. tostring(shader_lsp_enabled()),
		"shader lsp cmd: " .. lsp_cmd_label(),
		"shader extension dir: " .. tostring(find_installed_extension() or "<not found>"),
		"shader project root: " .. tostring(M.find_project_root(vim.api.nvim_buf_get_name(0)) or "<not found>"),
		"shader lsp attached: " .. tostring(has_attached_client(0)),
	}
end

return M
