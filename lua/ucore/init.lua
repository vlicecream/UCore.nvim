local M = {}

-- Configure UCore and register editor commands.
-- 配置 UCore，并注册编辑器命令。
function M.setup(opts)
	require("ucore.config").setup(opts)
	require("ucore.commands").register()
end

return M
