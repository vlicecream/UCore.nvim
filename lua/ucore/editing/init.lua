local config = require("ucore.config")

local M = {}

local filetypes = {
	unreal_cpp = true,
}

local function valid_buffer(bufnr)
	return bufnr and vim.api.nvim_buf_is_valid(bufnr) and vim.bo[bufnr].buftype == ""
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
		pcall(vim.cmd, "filetype plugin indent on")

		if config.values.editing.indent.inherit_cpp ~= false then
			pcall(vim.cmd, "runtime! indent/unreal_cpp.vim")
		end

		if config.values.editing.indent.fallback_cindent ~= false then
			vim.bo.autoindent = true
			vim.bo.cindent = true
		end
	end)
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

	return {
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
		"b:did_indent: " .. tostring(vim.b[bufnr].did_indent),
		"nvim-autopairs available: " .. tostring(ok_autopairs),
		"ucore autopairs rules: " .. tostring(rule_count),
		"insert <CR> map: " .. (type(cr_map) == "table" and tostring(cr_map.rhs or cr_map.callback or "") or tostring(cr_map)),
	}
end

function M.setup()
	if config.values.editing.enable == false then
		return
	end

	local group = vim.api.nvim_create_augroup("UCoreEditing", { clear = true })
	vim.api.nvim_create_autocmd("FileType", {
		group = group,
		pattern = "unreal_cpp",
		callback = function(ev)
			M.apply_indent(ev.buf)
		end,
	})

	vim.schedule(function()
		for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
			M.apply_indent(bufnr)
		end
	end)
end

return M
