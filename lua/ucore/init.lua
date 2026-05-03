local M = {}
local initialized = false

local function clear_augroup(name)
	pcall(vim.api.nvim_del_augroup_by_name, name)
end

local function setup_lifecycle_autocmds()
	local group = vim.api.nvim_create_augroup("UCoreLifecycle", { clear = true })
	vim.api.nvim_create_autocmd("VimLeavePre", {
		group = group,
		callback = function()
			pcall(function()
				require("ucore.server").stop()
			end)
		end,
	})
end

function M.reset()
	pcall(function()
		require("ucore.autocmd").reset()
	end)
	pcall(function()
		require("ucore.keymaps").reset()
	end)
	pcall(function()
		require("ucore.completion").reset()
	end)
	pcall(function()
		require("ucore.semantic").reset()
	end)
	pcall(function()
		require("ucore.diagnostics").reset()
	end)
	pcall(function()
		require("ucore.autosave").reset()
	end)
	pcall(function()
		require("ucore.output").reset()
	end)

	clear_augroup("UCoreAutopairs")
	clear_augroup("UCoreEditing")
	clear_augroup("UCoreLifecycle")
	pcall(vim.api.nvim_del_user_command, "UCore")
	initialized = false
end

-- Configure UCore, register commands, and setup optional autocmds.
-- 配置 UCore、注册命令，并设置可选的自动命令。
function M.setup(opts)
	if initialized then
		M.reset()
	end

	require("ucore.config").setup(opts)
	require("ucore.output").setup()
	require("ucore.commands").register()
	require("ucore.completion").setup()
	pcall(function()
		local lsp_config = require("ucore.config").values.lsp or {}
		if lsp_config.auto_setup ~= false then
			require("ucore.lsp").setup_clangd()
		end
	end)
	require("ucore.keymaps").setup()
	require("ucore.editing").setup()
	require("ucore.semantic").setup()
	require("ucore.diagnostics").setup()
	require("ucore.autocmd").setup()
	require("ucore.autosave").setup()
	setup_lifecycle_autocmds()
	pcall(function()
		require("ucore.autopairs").setup()
	end)
	initialized = true
end

return M
