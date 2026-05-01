local project = require("ucore.project")
local tree = require("ucore.explorer.tree")
local vcs = require("ucore.vcs")

local M = {}
local CACHE_TTL_MS = 10000
local cache = {}

local function now_ms()
	return vim.loop.hrtime() / 1000000
end

local function file_node(path, label, meta)
	return {
		id = "vcs-file:" .. tostring(path),
		label = label or vim.fn.fnamemodify(path, ":t"),
		path = path,
		type = "file",
		meta = meta or {},
		children = {},
	}
end

local function group_files(label, files, root)
	local children = {}
	for _, file in ipairs(files or {}) do
		local path = file.path
		if path then
			table.insert(children, file_node(path, vim.fn.fnamemodify(path, ":."), file))
		end
	end
	return tree.virtual_group(label, children, {
		id = "vcs-group:" .. label .. ":" .. tostring(root or ""),
	})
end

local function changelist_groups(provider, root)
	local key = provider.name() .. ":" .. tostring(root)
	local cached = cache[key]
	local opened
	if cached and cached.expires_at > now_ms() then
		opened = cached.opened
	else
		opened = provider.opened(root) or {}
		cache[key] = {
			opened = opened,
			expires_at = now_ms() + CACHE_TTL_MS,
		}
	end

	local by_change = {}
	for _, file in ipairs(opened) do
		local change = tostring(file.change or "default")
		by_change[change] = by_change[change] or {}
		table.insert(by_change[change], file)
	end

	local groups = {}
	local seen = {}

	if by_change.default and not seen.default then
		table.insert(groups, group_files("default", by_change.default, root))
	end

	for change, files in pairs(by_change) do
		if change ~= "default" and not seen[change] then
			table.insert(groups, group_files("CL " .. change, files, root))
		end
	end

	table.sort(groups, function(a, b)
		return a.label:lower() < b.label:lower()
	end)

	return groups
end

function M.load()
	local root = project.find_project_root_from_context()
	if not root then
		return tree.message("VCS", "No Unreal project detected")
	end

	local provider = vcs.detect(root)
	if not provider then
		return tree.virtual_group("VCS", {
			tree.message("Provider", "No VCS provider detected"),
			tree.message("Git", "Git provider placeholder"),
			tree.message("SVN", "SVN provider placeholder"),
		}, {
			id = "vcs-root:" .. root,
		})
	end

	if provider.name() ~= "p4" then
		return tree.virtual_group("VCS", {
			tree.message("Provider", provider.name():upper() .. " provider placeholder"),
		}, {
			id = "vcs-root:" .. root,
		})
	end

	return tree.virtual_group("P4", {
		tree.virtual_group("Opened Changelists", changelist_groups(provider, root), {
			id = "vcs-opened:" .. root,
		}),
		tree.virtual_group("Writable But Not Checked Out", {
			tree.message("Skipped", "Skipped in Explorer to avoid blocking; use :UCore vcs dashboard for full scan"),
		}, {
			id = "vcs-writable-skipped:" .. root,
		}),
	}, {
		id = "vcs-root:" .. root,
	})
end

return M
