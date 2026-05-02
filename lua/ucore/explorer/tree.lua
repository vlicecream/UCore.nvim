local config = require("ucore.config")

local M = {}

local function normalize(path)
	return path and path:gsub("\\", "/") or nil
end

local function basename(path)
	return vim.fn.fnamemodify(path, ":t")
end

local function explorer_config()
	return config.values.explorer or {}
end

local function should_skip(name, is_dir)
	local cfg = explorer_config()
	if name == "." or name == ".." then
		return true
	end
	if not cfg.show_hidden and name:sub(1, 1) == "." then
		return true
	end
	if is_dir then
		for _, excluded in ipairs(cfg.exclude_dirs or {}) do
			if name == excluded then
				return true
			end
		end
	end
	return false
end

local function sort_nodes(a, b)
	if a.type ~= b.type then
		return a.type == "directory"
	end
	return a.label:lower() < b.label:lower()
end

local function read_dir(path)
	local children = {}
	local ok, entries = pcall(vim.fn.readdir, path)
	if not ok then
		return {
			M.message("Read failed", "Cannot read directory: " .. tostring(path)),
		}
	end
	for _, name in ipairs(entries or {}) do
		local child_path = normalize(path .. "/" .. name)
		local is_dir = vim.fn.isdirectory(child_path) == 1
		if not should_skip(name, is_dir) then
			local node = {
				id = child_path,
				label = name,
				path = child_path,
				type = is_dir and "directory" or "file",
				children = {},
				loaded = not is_dir,
			}
			table.insert(children, node)
		end
	end
	table.sort(children, sort_nodes)
	return children
end

function M.from_path(path, opts)
	opts = opts or {}
	path = normalize(path)
	if not path or vim.fn.isdirectory(path) ~= 1 then
		return {
			id = opts.id or tostring(path or "missing"),
			label = opts.label or "Missing directory",
			type = "message",
			message = opts.empty_message or "Directory not found: " .. tostring(path),
			children = {},
		}
	end

	local lazy = opts.lazy == true

	return {
		id = opts.id or path,
		label = opts.label or basename(path),
		path = path,
		type = "directory",
		children = lazy and {} or read_dir(path),
		loaded = not lazy,
	}
end

function M.virtual_group(label, children, opts)
	opts = opts or {}
	return {
		id = opts.id or label,
		label = label,
		type = "directory",
		virtual = true,
		children = children or {},
		loaded = opts.loaded ~= false,
		load_children = opts.load_children,
	}
end

function M.message(label, message)
	return {
		id = label,
		label = label,
		type = "message",
		message = message or label,
		children = {},
	}
end

local function flatten_node(node, state, depth, out, prefix, is_last)
	if not node then
		return
	end

	table.insert(out, {
		node = node,
		depth = depth,
		prefix = prefix or "",
		is_last = is_last == true,
	})

	if node.type ~= "directory" then
		return
	end

	if depth > 0 and not state.is_expanded(node) then
		return
	end

	M.ensure_children(node)

	local children = node.children or {}
	local child_prefix = prefix or ""
	if depth > 0 then
		child_prefix = child_prefix .. (is_last and "   " or "│  ")
	end

	for index, child in ipairs(children) do
		flatten_node(child, state, depth + 1, out, child_prefix, index == #children)
	end
end

function M.ensure_children(node)
	if not node or node.type ~= "directory" or node.loaded then
		return
	end

	if type(node.load_children) == "function" then
		node.children = node.load_children(node) or {}
	elseif not node.virtual and node.path then
		node.children = read_dir(node.path)
	else
		node.children = node.children or {}
	end

	node.loaded = true
end

function M.expand_directory(node, state)
	if not node or node.type ~= "directory" then
		return
	end
	M.ensure_children(node)
	state.set_expanded(node, true)
end

function M.flatten(root, state)
	local out = {}
	flatten_node(root, state, 0, out, "", true)
	return out
end

function M.total_nodes(root)
	local total = 0
	local function walk(node)
		if not node then
			return
		end
		total = total + 1
		if node.type == "directory" and node.loaded == false then
			return
		end
		for _, child in ipairs(node.children or {}) do
			walk(child)
		end
	end
	walk(root)
	return total
end

function M.openable_path(node)
	if node and node.type == "file" and node.path and vim.fn.filereadable(node.path) == 1 then
		return node.path
	end
	return nil
end

return M
