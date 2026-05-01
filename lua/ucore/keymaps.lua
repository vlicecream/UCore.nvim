local config = require("ucore.config")
local navigation = require("ucore.navigation")
local project = require("ucore.project")

local M = {}

local group_name = "UCoreKeymaps"
local file_patterns = { "*.h", "*.hpp", "*.hh", "*.hxx", "*.inl", "*.cpp", "*.cc", "*.cxx" }
local filetypes = { "unreal_cpp", "cpp", "c" }

local function normalize_lhs(lhs)
	if lhs == false or lhs == nil or lhs == "" then
		return nil
	end

	return lhs
end

local function set_buffer_map(bufnr, lhs, rhs, desc)
	lhs = normalize_lhs(lhs)
	if not lhs then
		return
	end

	vim.keymap.set("n", lhs, rhs, {
		buffer = bufnr,
		desc = desc,
		silent = true,
	})
end

local function keymap_config()
	local navigation_config = config.values.navigation
	if type(navigation_config) ~= "table" then
		return {}
	end

	local keymaps = navigation_config.keymaps
	if type(keymaps) ~= "table" then
		return {}
	end

	return keymaps
end

local function setup_buffer(args)
	local keymaps = keymap_config()
	if keymaps.enable == false then
		return
	end

	local bufnr = args.buf
	local path = vim.api.nvim_buf_get_name(bufnr)
	if path == "" or not project.find_project_root(path) then
		return
	end

	set_buffer_map(bufnr, keymaps.definition or keymaps.goto_definition, navigation.goto_definition, "UCore definition")
	set_buffer_map(bufnr, keymaps.declaration or keymaps.global_declaration, navigation.goto_declaration, "UCore declaration")
	set_buffer_map(bufnr, keymaps.references, navigation.references, "UCore references")
	set_buffer_map(bufnr, keymaps.implementation or keymaps.goto_implementation, navigation.goto_implementation, "UCore implementation")
	set_buffer_map(bufnr, keymaps.source_toggle, navigation.toggle_source, "UCore toggle source/header")
end

-- Register gf for any buffer inside an Unreal project tree, including
-- .uproject, .uplugin, .build.cs, .ini, engine sources, etc.
-- gf 在任何 Unreal 项目文件内注册，包括 .uproject、引擎源码等。
local function setup_global_find(args)
	local keymaps = keymap_config()
	if keymaps.enable == false then
		return
	end

	local lhs = normalize_lhs(keymaps.global_find)
	if not lhs then
		return
	end

	local bufnr = args.buf
	vim.keymap.set("n", lhs, navigation.global_find, {
		buffer = bufnr,
		desc = "UCore global find",
		silent = true,
	})
end

local function is_unreal_path(path)
	if path == "" then
		return false
	end
	return project.find_project_root(path) ~= nil
end

function M.setup()
	local keymaps = keymap_config()
	if keymaps.enable == false then
		return
	end

	local group = vim.api.nvim_create_augroup(group_name, { clear = true })

	-- Broad: gf on every file that sits inside an Unreal project tree.
	-- 宽泛：项目内所有文件都注册 gf。
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile" }, {
		group = group,
		callback = function(args)
			local path = vim.api.nvim_buf_get_name(args.buf)
			if is_unreal_path(path) then
				setup_global_find(args)
			end
		end,
	})

	-- Restricted: navigation keymaps only for C++ files.
	-- 严格：导航快捷键仅限 C++ 文件。
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile" }, {
		group = group,
		pattern = file_patterns,
		callback = setup_buffer,
	})

	vim.api.nvim_create_autocmd("FileType", {
		group = group,
		pattern = filetypes,
		callback = setup_buffer,
	})
end

return M
