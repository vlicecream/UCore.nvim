local M = {}

-- Configure UCore, register commands, and setup optional autocmds.
-- 配置 UCore、注册命令，并设置可选的自动命令。
function M.setup(opts)
	require("ucore.config").setup(opts)
	require("ucore.commands").register()
	require("ucore.completion").setup()
	require("ucore.autocmd").setup()
end

return M
