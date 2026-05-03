local M = {}

-- Configure UCore, register commands, and setup optional autocmds.
-- 配置 UCore、注册命令，并设置可选的自动命令。
function M.setup(opts)
	require("ucore.config").setup(opts)
	require("ucore.commands").register()
	require("ucore.completion").setup()
	pcall(function()
		local lsp_config = require("ucore.config").values.lsp or {}
		if lsp_config.auto_setup ~= false then
			require("ucore.lsp").setup_clangd()
		end
	end)
	vim.schedule(function()
		pcall(function()
			require("ucore.bootstrap").prewarm_clangd()
		end)
	end)
	require("ucore.keymaps").setup()
	require("ucore.editing").setup()
	require("ucore.semantic").setup()
	require("ucore.diagnostics").setup()
	require("ucore.autocmd").setup()
	require("ucore.autosave").setup()
	require("ucore.debug").setup()
	pcall(function()
		require("ucore.autopairs").setup()
	end)
end

return M
