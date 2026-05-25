local completion = require("ucore.completion")
local config = require("ucore.config")
local debug = require("ucore.completion.debug")
local project = require("ucore.project")

local M = {}

-- Default blink source options.
-- blink source 默认配置。
local default_opts = {
	filetypes = {
		c = true,
		cpp = true,
		h = true,
		hpp = true,
		unreal_cpp = true,
	},
}

local latest_request_id = 0
local in_flight_request = nil
local queued_request = nil
local scheduled_timer = nil
local to_blink_item
local prune_items
local INSERT_TEXT_FORMAT_PLAIN_TEXT = 1
local ucore_filetype_sources = { "ucore" }

local function current_prefix(ctx)
	if type(ctx) == "table" then
		if type(ctx.get_keyword) == "function" then
			local ok, keyword = pcall(ctx.get_keyword, ctx)
			if ok and type(keyword) == "string" and keyword ~= "" then
				return keyword
			end
		end

		if type(ctx.line) == "string" and type(ctx.cursor) == "table" and type(ctx.bounds) == "table" then
			local start_col = tonumber(ctx.bounds.start_col)
			local cursor_col = tonumber(ctx.cursor[2])
			if start_col and cursor_col and cursor_col >= 0 then
				local keyword = ctx.line:sub(start_col, cursor_col)
				if keyword ~= "" then
					return keyword
				end
			end
		end

		local keyword = ctx.keyword or ctx.query
		if type(keyword) == "string" and keyword ~= "" then
			return keyword
		end
	end

	local row, col = unpack(vim.api.nvim_win_get_cursor(0))
	local line = vim.api.nvim_buf_get_lines(0, row - 1, row, false)[1] or ""
	return line:sub(1, col):match("[_%w]*$") or ""
end

local function blink_delay_ms()
	local completion_config = config.values.completion or {}
	local base = tonumber(completion_config.debounce_ms) or 180
	return math.max(30, math.min(base, 50))
end

local function stop_timer()
	if scheduled_timer then
		pcall(vim.fn.timer_stop, scheduled_timer)
		scheduled_timer = nil
	end
end

local function request_done(request)
	return not request or request.cancelled
end

local function dispatch_latest()
	if in_flight_request then
		debug.log("blink", "dispatch-skip-inflight")
		return
	end

	local request = queued_request
	queued_request = nil

	if request_done(request) then
		debug.log("blink", "dispatch-drop-cancelled")
		return
	end

	in_flight_request = request
	request.dispatch_ms = debug.now_ms()
	debug.log(
		"blink",
		"dispatch",
		string.format("id=%s", request.id),
		string.format("prefix=%s", request.prefix),
		string.format("queue_wait_ms=%s", debug.elapsed_ms(request.queued_ms))
	)

	completion.request({
		source = "blink",
		allow_stale = true,
	}, function(items, err)
		local request_ms = debug.elapsed_ms(request.dispatch_ms)
		local active = in_flight_request
		in_flight_request = nil

		if not active or active.id ~= request.id or active.cancelled then
			debug.log("blink", "drop-mismatched", string.format("id=%s", request.id))
			if queued_request and not queued_request.cancelled then
				dispatch_latest()
			end
			return
		end

		if err == "stale" then
			debug.log("blink", "stale", string.format("id=%s", request.id), string.format("request_ms=%s", request_ms))
			if queued_request and not queued_request.cancelled then
				dispatch_latest()
			end
			return
		end

		if err or not items then
			debug.log("blink", "error", string.format("id=%s", request.id), tostring(err), string.format("request_ms=%s", request_ms))
			request.callback({
				is_incomplete_forward = false,
				is_incomplete_backward = false,
				items = {},
			})
			if queued_request and not queued_request.cancelled then
				dispatch_latest()
			end
			return
		end

		local convert_started_ms = debug.now_ms()
		local blink_items = {}
		for _, item in ipairs(items) do
			local converted = to_blink_item(item)
			if converted then
				table.insert(blink_items, converted)
			end
		end
		local convert_ms = debug.elapsed_ms(convert_started_ms)

		debug.log(
			"blink",
			"items",
			string.format("id=%s", request.id),
			string.format("raw=%s", debug.count_items(items)),
			string.format("converted=%s", debug.count_items(blink_items)),
			string.format("request_ms=%s", request_ms),
			string.format("convert_ms=%s", convert_ms)
		)

		local prune_started_ms = debug.now_ms()
		blink_items = prune_items(blink_items)
		debug.log(
			"blink",
			"pruned",
			string.format("id=%s", request.id),
			string.format("items=%s", debug.count_items(blink_items)),
			string.format("prune_ms=%s", debug.elapsed_ms(prune_started_ms))
		)

		local callback_started_ms = debug.now_ms()
		request.callback({
			is_incomplete_forward = false,
			is_incomplete_backward = false,
			items = blink_items,
		})
		debug.log(
			"blink",
			"callback",
			string.format("id=%s", request.id),
			string.format("callback_ms=%s", debug.elapsed_ms(callback_started_ms)),
			string.format("total_ms=%s", debug.elapsed_ms(request.queued_ms))
		)

		if queued_request and not queued_request.cancelled then
			dispatch_latest()
		end
	end)
end

prune_items = function(items)
	local strong = 0
	for _, item in ipairs(items) do
		if (tonumber(item.score_offset) or 0) >= 10 then
			strong = strong + 1
		end
	end

	if strong < 12 then
		return items
	end

	local pruned = {}
	for _, item in ipairs(items) do
		if (tonumber(item.score_offset) or 0) >= 2 then
			table.insert(pruned, item)
		end
	end

	return #pruned > 0 and pruned or items
end

local function ensure_keymap_defaults(keymap)
	keymap = keymap or {}
	keymap.preset = keymap.preset or "enter"

	if keymap["<Tab>"] == nil then
		keymap["<Tab>"] = {
			function(cmp)
				if cmp.is_menu_visible() then
					return cmp.select_next()
				end
				if cmp.snippet_active() then
					return cmp.snippet_forward()
				end
			end,
			"fallback",
		}
	end

	if keymap["<S-Tab>"] == nil then
		keymap["<S-Tab>"] = {
			function(cmp)
				if cmp.is_menu_visible() then
					return cmp.select_prev()
				end
				if cmp.snippet_active() then
					return cmp.snippet_backward()
				end
			end,
			"fallback",
		}
	end

	if keymap["<CR>"] == nil then
		keymap["<CR>"] = { "accept", "fallback" }
	end

	return keymap
end

local function ensure_selection_defaults(completion_config)
	completion_config = completion_config or {}
	completion_config.list = completion_config.list or {}
	completion_config.list.selection = completion_config.list.selection or {}

	if completion_config.list.selection.preselect == nil then
		completion_config.list.selection.preselect = true
	end

	if completion_config.list.selection.auto_insert == nil then
		completion_config.list.selection.auto_insert = false
	end

	return completion_config
end

local function before_cursor()
	local row, col = unpack(vim.api.nvim_win_get_cursor(0))
	local line = vim.api.nvim_buf_get_lines(0, row - 1, row, false)[1] or ""
	return line:sub(1, col), col
end

local function in_include_context()
	local before = before_cursor()
	return before:match("^%s*#%s*include%s*[<\"][^>\"]*$") ~= nil
end

local function in_macro_context()
	local before = before_cursor()
	return before:match("%f[%w_]U[A-Z_]+%s*%([^)]*$") ~= nil
end

local function in_ucore_special_context()
	return in_include_context() or in_macro_context()
end

local function active_ucore_sources()
	if in_ucore_special_context() then
		return { "ucore" }
	end

	return vim.deepcopy(ucore_filetype_sources)
end

local function provider_should_show(prev)
	return function(ctx, items)
		if in_ucore_special_context() then
			return false
		end

		if type(prev) == "function" then
			return prev(ctx, items)
		end

		if prev == nil then
			return true
		end

		return prev
	end
end

function M.extend_blink_opts(opts)
	opts = opts or {}
	opts.sources = opts.sources or {}
	opts.sources.default = opts.sources.default or { "path", "snippets", "buffer" }
	opts.sources.per_filetype = opts.sources.per_filetype or {}
	opts.sources.providers = opts.sources.providers or {}
	for filetype, _ in pairs(default_opts.filetypes) do
		opts.sources.per_filetype[filetype] = active_ucore_sources
	end

	opts.sources.providers.ucore = vim.tbl_deep_extend("force", {
		name = "UCore",
		module = "ucore.completion.blink",
		async = true,
		timeout_ms = 8000,
		min_keyword_length = 0,
		score_offset = 50,
	}, opts.sources.providers.ucore or {})

	for _, provider in ipairs({ "buffer", "path", "snippets", "lsp" }) do
		local existing = opts.sources.providers[provider] or {}
		existing.should_show_items = provider_should_show(existing.should_show_items)
		opts.sources.providers[provider] = existing
	end

	opts.keymap = ensure_keymap_defaults(opts.keymap)
	opts.completion = ensure_selection_defaults(opts.completion)

	return opts
end

-- Create a blink.cmp source instance.
-- 创建 blink.cmp source 实例。
function M.new(opts)
	local self = setmetatable({}, {
		__index = M,
	})

	self.opts = vim.tbl_deep_extend("force", default_opts, opts or {})
	return self
end

-- Enable UCore completion only in C++-like buffers inside Unreal projects.
-- 只在 Unreal 工程里的 C++ 类 buffer 中启用 UCore 补全。
function M:enabled()
	if not self.opts.filetypes[vim.bo.filetype] then
		return false
	end

	return project.find_project_root() ~= nil
end

-- Trigger member completion after C++ member access operators.
-- 在 C++ 成员访问操作符后触发成员补全。
function M:get_trigger_characters()
	return {
		".",
		">",
		":",
		'"',
		"<",
		"/",
		"\\",
	}
end

-- Convert Vim complete-item shape into blink completion item shape.
-- 把 Vim complete-item 结构转换成 blink completion item 结构。
to_blink_item = function(item)
	local raw = item
	if type(item.user_data) == "string" and item.user_data ~= "" then
		local ok, decoded = pcall(vim.json.decode, item.user_data)
		if ok and type(decoded) == "table" then
			raw = vim.tbl_deep_extend("force", item, decoded)
		end
	end

	local label = raw.label or raw.name or raw.word or raw.insert_text or raw.insertText or raw.text or ""
	if label == "" then
		return nil
	end

	local insert_text = raw.insert_text or raw.insertText or raw.word or raw.name or label
	local kind = tonumber(raw.kind) or raw.kind
	local insert_text_format = tonumber(raw.insertTextFormat or raw.insert_text_format)
		or INSERT_TEXT_FORMAT_PLAIN_TEXT

	return {
		label = tostring(label),
		kind = kind,
		detail = raw.detail or raw.menu,
		labelDetails = raw.labelDetails,
		documentation = raw.documentation or raw.info,
		filterText = tostring(raw.filterText or raw.filter_text or label),
		insertText = tostring(insert_text),
		insertTextFormat = insert_text_format,
		textEdit = raw.textEdit or raw.text_edit,
		sortText = tostring(raw.sortText or raw.sort_text or label),
		score_offset = tonumber(raw.score_offset or raw.scoreOffset) or 0,
	}
end

-- Request completion candidates from the Rust backend.
-- 从 Rust 后端请求补全候选。
function M:get_completions(_, callback)
	if vim.b.no_cmp or vim.b.ucore_completion_disabled or vim.b.blink_cmp_disabled then
		debug.log("blink", "disabled")
		callback({
			is_incomplete_forward = false,
			is_incomplete_backward = false,
			items = {},
		})
		return
	end

	latest_request_id = latest_request_id + 1
	local request = {
		id = latest_request_id,
		prefix = current_prefix(_),
		callback = callback,
		cancelled = false,
		queued_ms = debug.now_ms(),
	}

	queued_request = request
	stop_timer()
	local delay_ms = blink_delay_ms()
	debug.log("blink", "queue", string.format("id=%s", request.id), string.format("prefix=%s", request.prefix), string.format("delay_ms=%s", delay_ms))
	scheduled_timer = vim.fn.timer_start(delay_ms, function()
		scheduled_timer = nil
		if in_flight_request then
			debug.log("blink", "timer-hit-inflight", string.format("id=%s", request.id))
			return
		end
		debug.log("blink", "timer-dispatch", string.format("id=%s", request.id), string.format("wait_ms=%s", debug.elapsed_ms(request.queued_ms)))
		dispatch_latest()
	end)

	return function()
		request.cancelled = true
		if queued_request and queued_request.id == request.id then
			queued_request = nil
		end
	end
end

return M
