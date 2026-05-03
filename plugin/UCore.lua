local function unload_ucore()
	local ok, existing = pcall(require, "ucore")
	if ok and type(existing) == "table" and type(existing.reset) == "function" then
		pcall(existing.reset)
	end

	for name, _ in pairs(package.loaded) do
		if name == "ucore" or name:match("^ucore%.") then
			package.loaded[name] = nil
		end
	end
end

if vim.g.loaded_ucore == 1 then
	unload_ucore()
else
	vim.g.loaded_ucore = 1
end

-- Load the Lua entry point and register commands.
-- 加载 Lua 入口并注册命令。
require("ucore").setup()

