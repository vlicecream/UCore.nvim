local M = {}

local configured = false
local dir_cache = {}

local extensions = {
	h = true,
	hpp = true,
	cpp = true,
	cc = true,
	cxx = true,
	inl = true,
}

local function normalize(path)
	if not path or path == "" then
		return nil
	end

	return vim.fn.fnamemodify(path, ":p"):gsub("\\", "/")
end

local function is_root(dir)
	return dir == nil or dir == "" or dir == "/" or dir:match("^%a:/$") ~= nil
end

local function cached_result_for(dir)
	local current = dir
	for _ = 1, 32 do
		if not current or current == "" then
			break
		end
		local cached = dir_cache[current]
		if cached ~= nil then
			return cached
		end

		local parent = vim.fn.fnamemodify(current, ":h"):gsub("\\", "/")
		if parent == current or is_root(current) then
			break
		end
		current = parent
	end

	return nil
end

local function set_cached(chain, value)
	for _, dir in ipairs(chain) do
		dir_cache[dir] = value
	end
end

local function is_unreal_buffer(path)
	local normalized = normalize(path)
	if not normalized then
		return false
	end

	local dir = vim.fn.fnamemodify(normalized, ":h"):gsub("\\", "/")
	local cached = cached_result_for(dir)
	if cached ~= nil then
		return cached
	end

	local visited = {}
	local current = dir
	for _ = 1, 32 do
		if not current or current == "" then
			break
		end

		table.insert(visited, current)
		local matches = vim.fn.glob(current .. "/*.uproject", false, true)
		if type(matches) == "table" and matches[1] then
			set_cached(visited, true)
			return true
		end

		if is_root(current) then
			break
		end

		local parent = vim.fn.fnamemodify(current, ":h"):gsub("\\", "/")
		if parent == current then
			break
		end
		current = parent
	end

	set_cached(visited, false)
	return false
end

function M.reset()
	dir_cache = {}
	configured = false
	pcall(vim.api.nvim_del_augroup_by_name, "UCoreFiletype")
end

local function should_retype(path, current_ft)
	local ext = path and path:match("%.([^.]+)$")
	if not ext or not extensions[ext:lower()] then
		return false
	end

	return current_ft == "" or current_ft == "c" or current_ft == "cpp" or current_ft == "objcpp"
end

local function apply_buffer_filetype(bufnr)
	if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local path = vim.api.nvim_buf_get_name(bufnr)
	if path == "" or not should_retype(path, vim.bo[bufnr].filetype) then
		return
	end

	if not is_unreal_buffer(path) then
		return
	end

	vim.api.nvim_buf_call(bufnr, function()
		vim.cmd("setfiletype unreal_cpp")
	end)
end

function M.setup()
	if configured then
		return
	end

	vim.filetype.add({
		extension = {
			h = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
			hpp = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
			cpp = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
			cc = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
			cxx = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
			inl = function(path)
				return is_unreal_buffer(path) and "unreal_cpp" or "cpp"
			end,
		},
	})

	local group = vim.api.nvim_create_augroup("UCoreFiletype", { clear = true })
	vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile", "BufEnter" }, {
		group = group,
		pattern = { "*.h", "*.hpp", "*.cpp", "*.cc", "*.cxx", "*.inl" },
		callback = function(ev)
			vim.schedule(function()
				apply_buffer_filetype(ev.buf)
			end)
		end,
	})

	vim.schedule(function()
		for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
			apply_buffer_filetype(bufnr)
		end
	end)

	configured = true
end

return M
