local project = require("ucore.project")
local config = require("ucore.config")
local remote = require("ucore.remote")

local M = {}
local auto_sequence = 0

-- Return true when the current mode can show insert completion.
-- 判断当前模式是否适合弹出插入模式补全菜单。
local function is_insert_mode()
	local mode = vim.api.nvim_get_mode().mode
	return mode == "i" or mode == "ic" or mode == "ix" or mode:sub(1, 2) == "ni"
end

-- Read current buffer content as one string.
-- 读取当前 buffer 内容，并合并成一个字符串。
local function current_content()
	local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
	return table.concat(lines, "\n") .. "\n"
end

-- Return text before cursor on the current line.
-- 返回当前行光标前面的文本。
local function before_cursor()
	local row, col = unpack(vim.api.nvim_win_get_cursor(0))
	local line = vim.api.nvim_buf_get_lines(0, row - 1, row, false)[1] or ""
	return line:sub(1, col), col
end

-- Find the byte index after the last member access operator.
-- 查找最后一个成员访问操作符之后的字节位置。
local function last_member_operator_end(text)
	local best = 0

	for _, pattern in ipairs({ "%-%>", "::", "%." }) do
		local index = 1

		while true do
			local _, finish = text:find(pattern, index)
			if not finish then
				break
			end

			if finish > best then
				best = finish
			end

			index = finish + 1
		end
	end

	return best
end

-- Return the identifier prefix being completed.
-- 返回当前正在补全的标识符前缀。
local function current_prefix()
	local before = before_cursor()
	local operator_end = last_member_operator_end(before)
	local segment = operator_end > 0 and before:sub(operator_end + 1) or before
	return segment:match("[_%w]*$") or ""
end

-- Find a conservative completion start column.
-- 查找一个保守的补全起始列。
local function completion_start_col()
	local before, col = before_cursor()
	local operator_end = last_member_operator_end(before)
	local segment = operator_end > 0 and before:sub(operator_end + 1) or before

	-- Only replace the currently typed identifier, not `this->` or `Super::`.
	-- 只替换当前正在输入的标识符，不替换 `this->` 或 `Super::`。
	local prefix = segment:match("[_%w]*$") or ""

	-- complete() expects 1-based byte column.
	-- complete() 需要 1-based 字节列。
	return col - #prefix + 1
end

-- Return true when the current text should trigger automatic completion.
-- 判断当前输入是否应该触发自动补全。
local function should_auto_trigger()
	local completion_config = config.values.completion or {}
	local min_chars = completion_config.min_chars or 2
	local before = before_cursor()

	if before:match("%-%>$") or before:match("::$") or before:match("%.$") then
		return true
	end

	local prefix = current_prefix()
	return #prefix >= min_chars and prefix:match("^[_%a][_%w]*$") ~= nil
end

-- Convert a Rust completion item into a Vim complete-item.
-- 把 Rust 补全项转换成 Vim complete-item。
local function to_complete_item(item)
	if type(item) == "string" then
		return {
			word = item,
			abbr = item,
		}
	end

	if type(item) ~= "table" then
		local text = tostring(item)
		return {
			word = text,
			abbr = text,
		}
	end

	local label = item.label or item.name or item.word or item.insert_text or item.insertText or item.text or ""

	local insert_text = item.insert_text or item.insertText or item.word or item.name or label

	local kind = item.kind or item.type or item.symbol_type or ""

	local detail = item.detail or item.menu or item.module_name or item.path or ""

	local documentation = item.documentation or item.info or item.path or ""

	return {
		word = tostring(insert_text),
		abbr = tostring(label),
		kind = tostring(kind),
		menu = tostring(detail),
		info = tostring(documentation),
		user_data = vim.json.encode(item),
	}
end

-- Normalize Rust completion response into Vim complete-items.
-- 将 Rust 补全响应规整成 Vim complete-items。
local function normalize_items(result)
	local raw_items = result

	if type(result) == "table" then
		raw_items = result.items or result.completions or result
	end

	local items = {}
	for _, item in ipairs(raw_items or {}) do
		table.insert(items, to_complete_item(item))
	end

	return items
end

-- Request completions from Rust for the current cursor position.
-- 根据当前光标位置向 Rust 请求补全。
function M.request(callback)
	callback = callback or function() end

	local root = project.find_project_root()
	if not root then
		return callback(nil, "Could not find .uproject")
	end

	local file_path = vim.api.nvim_buf_get_name(0)
	if file_path == "" then
		return callback(nil, "Current buffer has no file path")
	end

	local cursor = vim.api.nvim_win_get_cursor(0)
	local line = cursor[1] - 1
	local character = cursor[2]

	remote.get_completions(root, {
		content = current_content(),
		line = line,
		character = character,
		file_path = file_path:gsub("\\", "/"),
	}, function(result, err)
		if err then
			return callback(nil, err)
		end

		callback(normalize_items(result), nil)
	end)
end

-- Show native Vim insert completion menu.
-- 显示 Vim 原生插入模式补全菜单。
function M.complete(opts)
	opts = opts or {}

	if not is_insert_mode() then
		if not opts.silent then
			vim.notify("UCore complete must be triggered in Insert mode", vim.log.levels.WARN)
		end
		return
	end

	if opts.auto and not should_auto_trigger() then
		return
	end

	local start_col = completion_start_col()

	M.request(function(items, err)
		if err then
			if not opts.silent then
				vim.notify("UCore complete failed:\n" .. tostring(err), vim.log.levels.ERROR)
			end
			return
		end

		if not items or vim.tbl_isempty(items) then
			if not opts.silent then
				vim.notify("UCore complete: no candidates", vim.log.levels.INFO)
			end
			return
		end

		vim.schedule(function()
			if not is_insert_mode() then
				return
			end

			vim.fn.complete(start_col, items)
		end)
	end)
end

-- Schedule an automatic completion request with debounce.
-- 使用防抖调度一次自动补全请求。
local function schedule_auto_complete()
	local completion_config = config.values.completion or {}

	if vim.fn.pumvisible() == 1 then
		return
	end

	if not should_auto_trigger() then
		return
	end

	auto_sequence = auto_sequence + 1
	local sequence = auto_sequence
	local delay = completion_config.debounce_ms or 180

	vim.defer_fn(function()
		if sequence ~= auto_sequence then
			return
		end

		M.complete({
			auto = true,
			silent = true,
		})
	end, delay)
end

-- Setup automatic completion autocmds.
-- 设置自动补全自动命令。
function M.setup()
	local group = vim.api.nvim_create_augroup("UCoreCompletion", {
		clear = true,
	})

	vim.api.nvim_create_autocmd({ "TextChangedI", "TextChangedP" }, {
		group = group,
		callback = schedule_auto_complete,
	})
end

return M
