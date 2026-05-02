local completion = require("ucore.completion")
local log = require("ucore.log")
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

local policy_applied = false

local function preview_labels(items, limit)
	local labels = {}
	for _, item in ipairs(items or {}) do
		local label = item.label or item.word or item.abbr
		if label and label ~= "" then
			table.insert(labels, tostring(label))
		end

		if #labels >= limit then
			break
		end
	end

	return labels
end

local function call_or_value(value, ...)
	if type(value) == "function" then
		return value(...)
	end
	if value == nil then
		return true
	end
	return value
end

local function include_like_context(ctx)
	local line = type(ctx) == "table" and tostring(ctx.line or "") or ""
	local cursor = type(ctx) == "table" and type(ctx.cursor) == "table" and tonumber(ctx.cursor[2]) or #line
	local before = line:sub(1, math.max(cursor, 0))

	if before:match("^%s*#%s*include%s*[<\"][^>\"]*$") then
		return true
	end

	if before:match("[\"'][^\"']*[\\/]?[^\"']*$") then
		return true
	end

	return false
end

function M.apply_recommended_blink_policy()
	if policy_applied then
		return
	end

	local ok_config, blink_config = pcall(require, "blink.cmp.config")
	if not ok_config or type(blink_config) ~= "table" then
		return
	end

	local sources = blink_config.sources
	if type(sources) ~= "table" then
		return
	end

	sources.per_filetype = sources.per_filetype or {}
	if sources.per_filetype.unreal_cpp == nil then
		sources.per_filetype.unreal_cpp = {
			"ucore",
			"lsp",
			"snippets",
			"path",
			inherit_defaults = false,
		}
	end

	local providers = sources.providers or {}

	if type(providers.buffer) == "table" then
		local previous_enabled = providers.buffer.enabled
		providers.buffer.enabled = function()
			local enabled = call_or_value(previous_enabled)
			if not enabled then
				return false
			end
			return vim.bo.filetype ~= "unreal_cpp"
		end
	end

	if type(providers.path) == "table" then
		local previous_should_show = providers.path.should_show_items
		local previous_score_offset = providers.path.score_offset

		providers.path.should_show_items = function(ctx, items)
			local allowed = call_or_value(previous_should_show, ctx, items)
			if not allowed then
				return false
			end
			if vim.bo.filetype ~= "unreal_cpp" then
				return true
			end
			return include_like_context(ctx)
		end

		providers.path.score_offset = function(ctx, enabled_sources)
			local base = call_or_value(previous_score_offset, ctx, enabled_sources)
			base = tonumber(base) or 0
			if vim.bo.filetype == "unreal_cpp" then
				return base - 2
			end
			return base
		end
	end

	policy_applied = true
	log.write("completion.blink.policy", {
		filetype = "unreal_cpp",
		buffer_disabled = type(providers.buffer) == "table",
		path_filtered = type(providers.path) == "table",
	})

	local ok_blink, blink = pcall(require, "blink.cmp")
	if ok_blink and blink and blink.reload then
		pcall(blink.reload)
	end
end

local function prune_items(items)
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
	}
end

-- Convert Vim complete-item shape into LSP/blink completion item shape.
-- 把 Vim complete-item 结构转换成 LSP/blink completion item 结构。
local function to_blink_item(item)
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
		or vim.lsp.protocol.InsertTextFormat.PlainText

	return {
		label = tostring(label),
		kind = kind,
		detail = raw.detail or raw.menu,
		labelDetails = raw.labelDetails,
		documentation = raw.documentation or raw.info,
		filterText = tostring(raw.filterText or raw.filter_text or label),
		insertText = tostring(insert_text),
		insertTextFormat = insert_text_format,
		sortText = tostring(raw.sortText or raw.sort_text or label),
		score_offset = tonumber(raw.score_offset or raw.scoreOffset) or 0,
	}
end

-- Request completion candidates from the Rust backend.
-- 从 Rust 后端请求补全候选。
function M:get_completions(_, callback)
	if vim.b.no_cmp or vim.b.ucore_completion_disabled or vim.b.blink_cmp_disabled then
		log.write("completion.blink.skip", {
			reason = "disabled",
			no_cmp = vim.b.no_cmp == true,
			ucore_completion_disabled = vim.b.ucore_completion_disabled == true,
			blink_cmp_disabled = vim.b.blink_cmp_disabled == true,
		})
		callback({
			is_incomplete_forward = false,
			is_incomplete_backward = false,
			items = {},
		})
		return
	end

	local cancelled = false
	log.write("completion.blink.start", {
		filetype = vim.bo.filetype,
		prefix = vim.fn.expand("<cword>"),
	})

	completion.request(function(items, err)
		if cancelled then
			log.write("completion.blink.cancelled", {
				reason = "callback_after_cancel",
			})
			return
		end

		if err == "stale" then
			log.write("completion.blink.finish", {
				status = "stale",
			})
			return
		end

		if err or not items then
			log.write("completion.blink.finish", {
				status = "error",
				error = err,
			})
			callback({
				is_incomplete_forward = false,
				is_incomplete_backward = false,
				items = {},
			})
			return
		end

		local blink_items = {}
		for _, item in ipairs(items) do
			local converted = to_blink_item(item)
			if converted then
				table.insert(blink_items, converted)
			end
		end

		local converted_count = #blink_items
		blink_items = prune_items(blink_items)
		log.write("completion.blink.finish", {
			status = "ok",
			raw_count = #(items or {}),
			converted_count = converted_count,
			pruned_count = #blink_items,
			preview = preview_labels(blink_items, 8),
		})

		callback({
			is_incomplete_forward = false,
			is_incomplete_backward = false,
			items = blink_items,
		})
	end)

	return function()
		cancelled = true
		log.write("completion.blink.cancel", {
			reason = "provider_cancel",
		})
	end
end

return M
