local install = require("ucore.install")
local ui = require("ucore.ui")

local M = {}

local TITLE = "UCore Unreal Init"
local shown_roots = {}
local reopening = false

local function has_ui()
	return #(vim.api.nvim_list_uis() or {}) > 0
end

local function status_badge(ok, planned)
	if planned then
		return "[planned]"
	end
	return ok and "[ready]" or "[missing]"
end

local function build_items(project_root)
	local plugin = install.plugin_status(project_root)
	local nvr_ready = install.has_nvr()

	return {
		{
			key = "nvr",
			ready = nvr_ready,
			label = "nvr",
			badge = status_badge(nvr_ready, false),
			description = nvr_ready and "Reuse the current Neovim instance from Unreal" or "Install neovim-remote for Unreal -> Neovim reuse",
			run = function()
				local ok, result = install.install_nvr()
				vim.notify(
					ok and ("nvr ready: " .. tostring(result)) or ("nvr install failed:\n" .. tostring(result)),
					ok and vim.log.levels.INFO or vim.log.levels.WARN,
					{ title = TITLE }
				)
				return ok
			end,
		},
		{
			key = "plugin",
			ready = plugin.ready,
			label = "NvimSourceCodeAccess",
			badge = status_badge(plugin.ready, false),
			description = plugin.ready and tostring(plugin.message or "Unreal source accessor plugin is installed")
				or "Install the Unreal source accessor plugin into the current project",
			run = function()
				local ok, result = install.install_plugin("project")
				vim.notify(
					ok and ("Plugin installed:\n" .. tostring(result)) or ("Plugin install failed:\n" .. tostring(result)),
					ok and vim.log.levels.INFO or vim.log.levels.WARN,
					{ title = TITLE }
				)
				return ok
			end,
		},
		{
			key = "asset_jump",
			planned = true,
			ready = false,
			label = "Asset Jump Bridge",
			badge = status_badge(false, true),
			description = "Reserved: auto-start Unreal from Neovim and jump to indexed assets",
			run = function()
				vim.notify("Reserved for the future Unreal asset open flow.", vim.log.levels.INFO, {
					title = TITLE,
				})
				return false
			end,
		},
	}
end

local function should_auto_open(project_root)
	local items = build_items(project_root)
	for _, item in ipairs(items) do
		if not item.planned and not item.ready then
			return true
		end
	end
	return false
end

local function format_item(item)
	return string.format("%-22s %-10s %s", tostring(item.label or ""), tostring(item.badge or ""), tostring(item.description or ""))
end

function M.open(project_root, opts)
	opts = opts or {}
	if not has_ui() then
		return
	end

	if not opts.force and not should_auto_open(project_root) then
		return
	end

	local items = build_items(project_root)
	ui.select.items(TITLE, items, {
		format_item = format_item,
		on_choice = function(item)
			if not item or type(item.run) ~= "function" then
				return
			end

			local did_change = item.run()
			if not did_change then
				return
			end

			if should_auto_open(project_root) then
				reopening = true
				vim.defer_fn(function()
					reopening = false
					M.open(project_root, { force = true })
				end, 100)
			end
		end,
	})
end

function M.maybe_open(project_root)
	if not has_ui() or reopening then
		return
	end

	project_root = tostring(project_root or "")
	if project_root == "" or shown_roots[project_root] then
		return
	end

	if not should_auto_open(project_root) then
		return
	end

	shown_roots[project_root] = true
	vim.defer_fn(function()
		M.open(project_root)
	end, 120)
end

function M.reset()
	shown_roots = {}
	reopening = false
end

return M
