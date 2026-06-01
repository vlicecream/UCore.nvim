local config = require("ucore.config")

local M = {}

local filetypes = {
	unreal_cpp = true,
}

local function valid_buffer(bufnr)
	return bufnr and vim.api.nvim_buf_is_valid(bufnr) and vim.bo[bufnr].buftype == ""
end

local function apply_indent_globals()
	local indent = config.values.editing.indent or {}
	vim.g.ucore_indent_unreal_macro = indent.unreal_macro_keep_indent == false and 0 or 1
	vim.g.ucore_indent_semicolon = indent.semicolon_keep_indent == false and 0 or 1
end

function M.apply_indent(bufnr)
	if config.values.editing.indent.enable == false then
		return
	end

	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not valid_buffer(bufnr) or not filetypes[vim.bo[bufnr].filetype] then
		return
	end

	vim.api.nvim_buf_call(bufnr, function()
		apply_indent_globals()
		pcall(vim.cmd, "filetype plugin indent on")

		if config.values.editing.indent.inherit_cpp ~= false then
			pcall(vim.cmd, "runtime! indent/unreal_cpp.vim")
		end

		-- Keep the buffer on indentexpr-driven indentation so `;` and
		-- closed Unreal macros do not trigger an extra cindent pass.
		vim.bo[bufnr].autoindent = true
		vim.bo[bufnr].smartindent = false
		vim.bo[bufnr].cindent = config.values.editing.indent.fallback_cindent == true
		vim.bo[bufnr].indentkeys = "0{,0},0),0],!^F,o,O,e"
	end)
end

function M.apply_autoformat_guard(bufnr)
	if config.values.editing.disable_autoformat == false then
		return
	end

	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not valid_buffer(bufnr) or not filetypes[vim.bo[bufnr].filetype] then
		return
	end

	-- LazyVim and many format-on-save setups honor one of these buffer flags.
	-- This keeps Unreal projects from being reformatted by clang-format/conform
	-- unless the user explicitly opts back in.
	vim.b[bufnr].autoformat = false
	vim.b[bufnr].disable_autoformat = true
	vim.b[bufnr].ucore_autoformat_disabled = true
end

function M.apply_buffer_settings(bufnr)
	M.apply_indent(bufnr)
	M.apply_autoformat_guard(bufnr)
end

function M.info(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	local cr_map = vim.fn.maparg("<CR>", "i", false, true)
	local ok_autopairs = pcall(require, "nvim-autopairs")
	local npairs = ok_autopairs and require("nvim-autopairs") or nil
	local rule_count = 0
	if npairs and npairs.config and npairs.config.rules then
		for _, rule in ipairs(npairs.config.rules) do
			if rule._ucore then
				rule_count = rule_count + 1
			end
		end
	end

	local lines = {
		"buffer: " .. tostring(bufnr),
		"name: " .. vim.api.nvim_buf_get_name(bufnr),
		"filetype: " .. tostring(vim.bo[bufnr].filetype),
		"autoindent: " .. tostring(vim.bo[bufnr].autoindent),
		"cindent: " .. tostring(vim.bo[bufnr].cindent),
		"smartindent: " .. tostring(vim.bo[bufnr].smartindent),
		"indentexpr: " .. tostring(vim.bo[bufnr].indentexpr),
		"indentkeys: " .. tostring(vim.bo[bufnr].indentkeys),
		"shiftwidth: " .. tostring(vim.bo[bufnr].shiftwidth),
		"expandtab: " .. tostring(vim.bo[bufnr].expandtab),
		"formatoptions: " .. tostring(vim.bo[bufnr].formatoptions),
		"b:autoformat: " .. tostring(vim.b[bufnr].autoformat),
		"b:disable_autoformat: " .. tostring(vim.b[bufnr].disable_autoformat),
		"b:ucore_autoformat_disabled: " .. tostring(vim.b[bufnr].ucore_autoformat_disabled),
		"b:did_indent: " .. tostring(vim.b[bufnr].did_indent),
		"g:ucore_indent_unreal_macro: " .. tostring(vim.g.ucore_indent_unreal_macro),
		"g:ucore_indent_semicolon: " .. tostring(vim.g.ucore_indent_semicolon),
		"nvim-autopairs available: " .. tostring(ok_autopairs),
		"ucore autopairs rules: " .. tostring(rule_count),
		"insert <CR> map: " .. (type(cr_map) == "table" and tostring(cr_map.rhs or cr_map.callback or "") or tostring(cr_map)),
	}

	local expected = vim.bo[bufnr].filetype == "unreal_cpp"
		and vim.bo[bufnr].indentexpr == "GetUnrealCppIndent()"
		and vim.bo[bufnr].cindent == false
		and not tostring(vim.bo[bufnr].indentkeys):find(";", 1, true)
	if not expected then
		table.insert(lines, "status: mismatch")
		table.insert(lines, "hint: run :UCore editing fix")
	else
		table.insert(lines, "status: ok")
	end

	return lines
end

function M.fix(bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	M.apply_buffer_settings(bufnr)
	return M.info(bufnr)
end

function M.setup()
	if config.values.editing.enable == false then
		return
	end

	apply_indent_globals()
	local group = vim.api.nvim_create_augroup("UCoreEditing", { clear = true })
	vim.api.nvim_create_autocmd({ "FileType", "BufEnter" }, {
		group = group,
		pattern = "unreal_cpp",
		callback = function(ev)
			vim.schedule(function()
				if vim.api.nvim_buf_is_valid(ev.buf) then
					M.apply_buffer_settings(ev.buf)
				end
			end)
		end,
	})

	vim.schedule(function()
		for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
			M.apply_buffer_settings(bufnr)
		end
	end)
end

return M
