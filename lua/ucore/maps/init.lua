local project = require("ucore.project")
local remote = require("ucore.remote")

local M = {}

-- Decode JSON arrays stored as strings by the Rust database layer.
-- 解码 Rust 数据库层以字符串形式返回的 JSON 数组。
local function decode_json_array(value)
	if type(value) ~= "string" or value == "" then
		return value
	end

	local ok, decoded = pcall(vim.json.decode, value)
	if ok and type(decoded) == "table" then
		return decoded
	end

	return value
end

-- Build convenient lookup maps from component/module query results.
-- 根据 components/modules 查询结果构造方便 Lua 使用的映射表。
function M.get_maps(start_path, on_complete)
	local project_root = project.find_project_root(start_path)
	if not project_root then
		return on_complete(false, "Could not find .uproject")
	end

	remote.get_components(project_root, function(components, err)
		if err then
			return on_complete(false, err)
		end

		remote.get_modules(project_root, function(modules, err2)
			if err2 then
				return on_complete(false, err2)
			end

			local all_components_map = {}
			local all_modules_map = {}
			local module_to_component_name = {}

			-- Index components by name for quick lookup.
			-- 按名称索引 component，方便快速查找。
			for _, comp in ipairs(components or {}) do
				all_components_map[comp.name] = comp
			end

			-- Normalize module rows into stable Lua metadata.
			-- 把 module row 规整成稳定的 Lua 元数据结构。
			for _, row in ipairs(modules or {}) do
				local mod = {
					name = tostring(row.name),
					type = tostring(row.type or ""),
					scope = tostring(row.scope or ""),
					module_root = tostring(row.module_root or ""),
					path = row.build_cs_path and tostring(row.build_cs_path) or nil,
					owner_name = tostring(row.owner_name or ""),
					component_name = tostring(row.component_name or ""),
					deep_dependencies = decode_json_array(row.deep_dependencies),
				}

				all_modules_map[mod.name] = mod
				module_to_component_name[mod.name] = mod.component_name
			end

			on_complete(true, {
				project_root = project_root,
				all_components_map = all_components_map,
				all_modules_map = all_modules_map,
				module_to_component_name = module_to_component_name,
			})
		end)
	end)
end

return M
