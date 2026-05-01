local config = require("ucore.config")

local M = {}

local function normalize(text)
	if (config.values.explorer or {}).search_case_sensitive then
		return tostring(text or "")
	end
	return tostring(text or ""):lower()
end

local function node_text(node)
	return table.concat({
		node.label or "",
		node.path or "",
		node.message or "",
	}, " ")
end

local function matches(node, query)
	if query == "" then
		return true
	end
	return normalize(node_text(node)):find(normalize(query), 1, true) ~= nil
end

local function filter_node(node, query, expanded, matched_count)
	if not node then
		return nil, matched_count
	end

	local child_matches = {}
	for _, child in ipairs(node.children or {}) do
		local filtered
		filtered, matched_count = filter_node(child, query, expanded, matched_count)
		if filtered then
			table.insert(child_matches, filtered)
		end
	end

	local self_match = matches(node, query)
	if self_match then
		matched_count = matched_count + 1
	end

	if query == "" or self_match or #child_matches > 0 then
		local clone = vim.tbl_extend("force", {}, node)
		clone.children = query == "" and node.children or child_matches
		if query ~= "" and #child_matches > 0 then
			expanded[clone.id or clone.path or clone.label] = true
		end
		return clone, matched_count
	end

	return nil, matched_count
end

function M.apply(root, query)
	local expanded = {}
	local filtered, matched = filter_node(root, query or "", expanded, 0)
	return filtered or root, matched, expanded
end

return M
