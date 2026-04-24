-- Avoid loading the plugin more than once.
-- 避免插件被重复加载。
if vim.g.loaded_ucore == 1 then
	return
end

vim.g.loaded_ucore = 1

-- Register Unreal tree-sitter support as early as possible.
-- 尽早注册 Unreal tree-sitter 支持，避免后续安装阶段看不到语言。
pcall(function()
	require("ucore.treesitter").setup()
end)

-- Load the Lua entry point and register commands.
-- 加载 Lua 入口并注册命令。
require("ucore").setup()

