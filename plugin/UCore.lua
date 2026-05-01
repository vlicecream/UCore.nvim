-- Avoid loading the plugin more than once.
-- 避免插件被重复加载。
if vim.g.loaded_ucore == 1 then
	return
end

vim.g.loaded_ucore = 1

-- Load the Lua entry point and register commands.
-- 加载 Lua 入口并注册命令。
require("ucore").setup()

