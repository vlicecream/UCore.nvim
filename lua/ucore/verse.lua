local config = require("ucore.config")

local M = {}
local group_name = "UCoreVerse"
local client_name = "verse_lsp"
local uv = vim.uv or vim.loop

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

-- Return the configured Verse feature block.
-- 返回配置中的 Verse 功能块。
local function verse_config()
	return config.values.verse or {}
end

-- Return the configured Verse LSP block.
-- 返回配置中的 Verse LSP 配置块。
local function verse_lsp_config()
	return verse_config().lsp or {}
end

-- Return whether Verse support is enabled.
-- 返回是否启用了 Verse 支持。
local function verse_enabled()
	return verse_config().enable ~= false
end

-- Return whether Verse LSP support is enabled.
-- 返回是否启用了 Verse LSP 支持。
local function verse_lsp_enabled()
	return verse_enabled() and verse_lsp_config().enable ~= false
end

-- Return whether the path points to a Verse source file.
-- 返回路径是否指向 Verse 源文件。
local function is_verse_path(path)
	path = normalize(path)
	return path and path:match("%.verse$") ~= nil or false
end

-- Return whether the path looks like a Verse project marker file.
-- 返回路径是否看起来像 Verse 项目标记文件。
local function is_verse_project_marker(path)
	path = normalize(path)
	return path and (path:match("%.vproject$") or path:match("%.uefnproject$")) ~= nil or false
end

-- Return the home directories used while searching installed Verse extensions.
-- 返回搜索已安装 Verse 扩展时使用的主目录列表。
local function home_dirs()
	local dirs = {}
	local home = normalize(vim.fn.expand("$HOME"))
	if home and home ~= "" then
		dirs[#dirs + 1] = home
	end
	for _, item in ipairs(verse_lsp_config().extra_home_dirs or {}) do
		local path = normalize(item)
		if path and path ~= "" then
			dirs[#dirs + 1] = path
		end
	end
	return dirs
end

-- Return the extension parent directories searched for Epic's Verse extension.
-- 返回搜索 Epic Verse 扩展时使用的扩展父目录列表。
local function extension_parent_dirs()
	local dirs = {}
	for _, home in ipairs(home_dirs()) do
		local vscode = normalize(home .. "/.vscode/extensions")
		local cursor = normalize(home .. "/.cursor/extensions")
		local antigravity = normalize(home .. "/.antigravity/extensions")
		if vscode then
			dirs[#dirs + 1] = vscode
		end
		if cursor then
			dirs[#dirs + 1] = cursor
		end
		if antigravity then
			dirs[#dirs + 1] = antigravity
		end
	end

	for _, item in ipairs(verse_lsp_config().extension_dirs or {}) do
		local path = normalize(item)
		if path then
			dirs[#dirs + 1] = path
		end
	end

	return dirs
end

-- Return the matching extension directories under one parent directory.
-- 返回一个父目录下匹配的扩展目录列表。
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

		if kind == "directory" and name:match("^epicgames%.verse") then
			table.insert(dirs, normalize(base .. "/" .. name))
		end
	end

	return dirs
end

-- Find the newest installed Epic Verse VS Code extension directory.
-- 查找最新安装的 Epic Verse VS Code 扩展目录。
local function find_installed_extension()
	local matches = {}
	for _, base in ipairs(extension_parent_dirs()) do
		for _, dir in ipairs(matching_extension_dirs(base)) do
			table.insert(matches, dir)
		end
	end

	table.sort(matches, function(left, right)
		return tostring(left) > tostring(right)
	end)

	return matches[1]
end

-- Return the platform-specific Verse LSP binary candidates for one extension dir.
-- 返回某个扩展目录下按平台推导出的 Verse LSP 可执行候选。
local function lsp_binary_candidates(ext_dir)
	local candidates = {}
	ext_dir = normalize(ext_dir)
	if not ext_dir then
		return candidates
	end

	local sysname = (uv.os_uname() or {}).sysname
	if sysname == "Windows_NT" then
		table.insert(candidates, normalize(ext_dir .. "/bin/Win64/verse-lsp.exe"))
	elseif sysname == "Darwin" then
		table.insert(candidates, normalize(ext_dir .. "/bin/Mac/verse-lsp"))
	elseif sysname == "Linux" then
		table.insert(candidates, normalize(ext_dir .. "/bin/Linux/verse-lsp"))
	end

	table.insert(candidates, normalize(ext_dir .. "/bin/verse-lsp"))
	table.insert(candidates, normalize(ext_dir .. "/verse-lsp"))
	return candidates
end

-- Find the Verse LSP binary path from config overrides or installed extensions.
-- 从配置覆盖或已安装扩展中查找 Verse LSP 可执行路径。
local function find_lsp_binary()
	local cmd = verse_lsp_config().cmd
	if type(cmd) == "string" and cmd ~= "" then
		return normalize(cmd)
	end
	if type(cmd) == "table" and type(cmd[1]) == "string" and cmd[1] ~= "" then
		return normalize(cmd[1])
	end

	local ext_dir = find_installed_extension()
	if not ext_dir then
		return nil
	end

	for _, candidate in ipairs(lsp_binary_candidates(ext_dir)) do
		if path_exists(candidate) then
			return candidate
		end
	end

	return nil
end

-- Return the full Verse LSP command array.
-- 返回完整的 Verse LSP 命令数组。
local function lsp_cmd()
	local cmd = verse_lsp_config().cmd
	if type(cmd) == "table" and #cmd > 0 then
		return vim.deepcopy(cmd)
	end
	if type(cmd) == "string" and cmd ~= "" then
		return { cmd }
	end

	local binary = find_lsp_binary()
	if binary then
		return { binary }
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

-- Find the Verse project root by walking upward from the given path.
-- 从给定路径向上查找 Verse 项目根目录。
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
		return name == ".vproject" or name:match("%.uefnproject$")
	end, {
		path = dir,
		upward = true,
		type = "file",
		limit = 1,
	})

	if markers[1] then
		return normalize(vim.fs.dirname(markers[1]))
	end

	return nil
end

-- Apply Verse filetype detection to one buffer when it matches known extensions.
-- 在缓冲区匹配已知扩展时应用 Verse 文件类型检测。
local function apply_buffer_filetype(bufnr)
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local path = vim.api.nvim_buf_get_name(bufnr)
	if path == "" then
		return
	end

	if is_verse_path(path) and vim.bo[bufnr].filetype ~= "verse" then
		vim.api.nvim_buf_call(bufnr, function()
			vim.cmd("setfiletype verse")
		end)
	end
end

-- Return whether a Verse LSP client is already attached to the buffer.
-- 返回 Verse LSP 客户端是否已经附加到该缓冲区。
local function has_attached_client(bufnr)
	if type(vim.lsp.get_clients) ~= "function" then
		return false
	end

	local clients = vim.lsp.get_clients({ bufnr = bufnr })
	for _, client in ipairs(clients or {}) do
		if client.name == client_name then
			return true
		end
	end

	return false
end

-- Stop active Verse LSP clients attached to the current Neovim instance.
-- 停止当前 Neovim 实例里激活的 Verse LSP 客户端。
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

-- Restart the Verse LSP client for the current buffer when possible.
-- 在可行时为当前缓冲区重启 Verse LSP 客户端。
function M.restart_lsp()
	local stopped = M.stop_lsp()
	start_lsp(vim.api.nvim_get_current_buf())
	return stopped
end

-- Start the Verse LSP client for one buffer when the environment is ready.
-- 在环境就绪时为单个缓冲区启动 Verse LSP 客户端。
local function start_lsp(bufnr)
	if not verse_lsp_enabled() or type(vim.lsp.start) ~= "function" then
		return
	end
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) or vim.bo[bufnr].filetype ~= "verse" then
		return
	end
	if has_attached_client(bufnr) then
		return
	end

	local cmd = lsp_cmd()
	if not cmd or #cmd == 0 then
		return
	end

	local path = vim.api.nvim_buf_get_name(bufnr)
	local root = M.find_project_root(path) or vim.fs.dirname(path)
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
end

-- Open hover information through the active Verse LSP client.
-- 通过当前 Verse LSP 客户端打开 hover 信息。
function M.hover()
	if vim.bo.filetype ~= "verse" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.hover()
end

-- Open signature help through the active Verse LSP client.
-- 通过当前 Verse LSP 客户端打开签名帮助。
function M.signature_help()
	if vim.bo.filetype ~= "verse" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.signature_help()
end

-- Jump to the definition through the active Verse LSP client.
-- 通过当前 Verse LSP 客户端跳转到定义。
function M.definition()
	if vim.bo.filetype ~= "verse" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.definition()
end

-- Find references through the active Verse LSP client.
-- 通过当前 Verse LSP 客户端查找引用。
function M.references()
	if vim.bo.filetype ~= "verse" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.references()
end

-- Rename the symbol through the active Verse LSP client.
-- 通过当前 Verse LSP 客户端重命名符号。
function M.rename()
	if vim.bo.filetype ~= "verse" or not has_attached_client(0) then
		return
	end
	vim.lsp.buf.rename()
end

-- Register filetype detection and LSP startup hooks for Verse buffers.
-- 为 Verse 缓冲区注册文件类型检测和 LSP 启动钩子。
function M.setup()
	if not verse_enabled() then
		return
	end

	vim.filetype.add({
		extension = {
			verse = "verse",
		},
		pattern = {
			[".*%.vproject"] = "json",
			[".*%.uefnproject"] = "json",
		},
	})

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufEnter" }, {
		group = group,
		pattern = "*.verse",
		callback = function(ev)
			vim.schedule(function()
				apply_buffer_filetype(ev.buf)
			end)
		end,
	})

	vim.api.nvim_create_autocmd("FileType", {
		group = group,
		pattern = "verse",
		callback = function(ev)
			if verse_lsp_config().auto_start ~= false then
				vim.schedule(function()
					start_lsp(ev.buf)
				end)
			end
		end,
	})
end

-- Reset Verse-specific autocmds.
-- 重置 Verse 相关的自动命令。
function M.reset()
	pcall(vim.api.nvim_del_augroup_by_name, group_name)
end

-- Return a summary of the current Verse integration state.
-- 返回当前 Verse 集成状态摘要。
function M.info()
	return {
		"verse enabled: " .. tostring(verse_enabled()),
		"verse lsp enabled: " .. tostring(verse_lsp_enabled()),
		"verse lsp cmd: " .. lsp_cmd_label(),
		"verse extension dir: " .. tostring(find_installed_extension() or "<not found>"),
		"verse project root: " .. tostring(M.current_project_root() or "<not found>"),
		"verse lsp attached: " .. tostring(has_attached_client(0)),
	}
end

-- Return whether the current buffer belongs to a Verse project.
-- 返回当前缓冲区是否属于 Verse 项目。
function M.current_project_root()
	local path = vim.api.nvim_buf_get_name(0)
	if path == "" then
		return nil
	end

	if is_verse_project_marker(path) then
		return normalize(vim.fs.dirname(path))
	end

	return M.find_project_root(path)
end

return M
