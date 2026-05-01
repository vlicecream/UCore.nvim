local completion = require("ucore.completion")
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
	}
end

-- Request completion candidates from the Rust backend.
-- 从 Rust 后端请求补全候选。
function M:get_completions(_, callback)
  if vim.b.no_cmp or vim.b.ucore_completion_disabled or vim.b.blink_cmp_disabled then
    callback({
      is_incomplete_forward = false,
      is_incomplete_backward = false,
      items = {},
    })
    return
  end

  local cancelled = false

  completion.request(function(items, err)
		if cancelled then
			return
		end

		if err == "stale" then
			return
		end

		if err or not items then
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

		callback({
			is_incomplete_forward = false,
			is_incomplete_backward = false,
			items = blink_items,
		})
	end)

	return function()
		cancelled = true
	end
end

return M
