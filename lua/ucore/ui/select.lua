local M = {}

-- Open a generic selection UI with a label formatter.
-- 打开一个通用选择 UI，并支持自定义显示文本。
local function pick(title, items, format_item, on_choice)
	if type(items) ~= "table" or vim.tbl_isempty(items) then
		vim.notify(title .. ": no results", vim.log.levels.WARN)
		return
	end

	vim.ui.select(items, {
		prompt = title,
		format_item = format_item,
	}, function(choice)
		if not choice then
			return
		end

		on_choice(choice)
	end)
end

-- Pick a module and open its Build.cs or module root.
-- 选择一个模块，并打开它的 Build.cs 或模块目录。
function M.modules(modules)
	pick("UCore modules", modules, function(item)
		local name = tostring(item.name or "<unknown>")
		local typ = tostring(item.type or "")
		local owner = tostring(item.owner_name or item.component_name or "")

		if owner ~= "" then
			return string.format("%s [%s] - %s", name, typ, owner)
		end

		return string.format("%s [%s]", name, typ)
	end, function(item)
		local target = item.build_cs_path or item.path or item.module_root

		if not target or target == vim.NIL or target == "" then
			vim.notify("Selected module has no path", vim.log.levels.WARN)
			return
		end

		-- Prefer opening files directly; directories are shown as a path for now.
		-- 优先直接打开文件；目录路径先只打印，后面可以接文件树/picker。
		if vim.fn.filereadable(target) == 1 then
			vim.cmd.edit(vim.fn.fnameescape(target))
		else
			print(target)
			vim.fn.setreg("+", target)
			vim.notify("Copied module path to clipboard")
		end
	end)
end

-- Pick an asset path and copy it to the clipboard.
-- 选择一个资产路径，并复制到剪贴板。
function M.assets(assets)
	pick("UCore assets", assets, function(item)
		return tostring(item)
	end, function(item)
		local asset_path = tostring(item)
		vim.fn.setreg("+", asset_path)
		vim.notify("Copied asset path: " .. asset_path)
	end)
end

-- Pick a symbol and open its source file when possible.
-- 选择一个符号，并尽量打开它所在的源码文件。
function M.symbols(symbols)
	pick("UCore symbols", symbols, function(item)
		local name = tostring(item.name or "<unknown>")
		local kind = tostring(item.symbol_type or item.type or "")
		local path = tostring(item.path or "")

		if path ~= "" then
			return string.format("%s [%s] - %s", name, kind, path)
		end

		return string.format("%s [%s]", name, kind)
	end, function(item)
		local path = item.path
		local line = tonumber(item.line or item.line_number or 1) or 1

		if path and path ~= vim.NIL and vim.fn.filereadable(path) == 1 then
			vim.cmd.edit(vim.fn.fnameescape(path))
			vim.api.nvim_win_set_cursor(0, { line, 0 })
		else
			print(vim.inspect(item))
		end
	end)
end

return M
