local M = {}

local function normalize_path(path)
	return path and path:gsub("\\", "/") or ""
end

local function find_buffer_for_path(path)
	path = normalize_path(path)
	if path == "" then
		return nil
	end

	for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
		if vim.api.nvim_buf_is_valid(bufnr) and normalize_path(vim.api.nvim_buf_get_name(bufnr)) == path then
			return bufnr
		end
	end

	return nil
end

local function ensure_buffer_open(path)
	local current = vim.api.nvim_get_current_buf()
	if vim.api.nvim_buf_is_valid(current) and normalize_path(vim.api.nvim_buf_get_name(current)) == path then
		return current
	end

	local existing = find_buffer_for_path(path)
	if existing and vim.api.nvim_buf_is_valid(existing) then
		if not vim.api.nvim_buf_is_loaded(existing) then
			pcall(vim.fn.bufload, existing)
		end
		vim.api.nvim_set_current_buf(existing)
		return existing
	end

	vim.cmd.edit(vim.fn.fnameescape(path))
	return vim.api.nvim_get_current_buf()
end

function M.refresh_filetype(path, bufnr)
	bufnr = bufnr or vim.api.nvim_get_current_buf()
	if not vim.api.nvim_buf_is_valid(bufnr) then
		return
	end

	local ok_utreesitter, utreesitter_filetype = pcall(require, "utreesitter.filetype")
	if ok_utreesitter and utreesitter_filetype and type(utreesitter_filetype.apply_to_buffer) == "function" then
		pcall(utreesitter_filetype.apply_to_buffer, bufnr)
		return
	end

	local detected = vim.filetype.match({ buf = bufnr, filename = path }) or vim.filetype.match({ filename = path })
	if detected and detected ~= "" and vim.bo[bufnr].filetype ~= detected then
		vim.bo[bufnr].filetype = detected
	end
end

function M.open_location(path, line, col, opts)
	opts = opts or {}
	path = normalize_path(path)

	if path == "" then
		if not opts.silent then
			vim.notify("UCore open: missing file path", vim.log.levels.WARN)
		end
		return false
	end

	if vim.fn.filereadable(path) ~= 1 then
		if not opts.silent then
			vim.notify("UCore open: file is not readable: " .. tostring(path), vim.log.levels.WARN)
		end
		return false
	end

	local ok, bufnr = pcall(ensure_buffer_open, path)
	if not ok or not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
		if not opts.silent then
			vim.notify("UCore open failed: " .. tostring(bufnr), vim.log.levels.ERROR)
		end
		return false
	end

	M.refresh_filetype(path, bufnr)

	local last_line = math.max(1, vim.api.nvim_buf_line_count(0))
	line = tonumber(line) or 1
	col = tonumber(col) or 0
	line = math.max(1, math.min(line, last_line))

	local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
	col = math.max(0, math.min(col, #line_text))

	vim.api.nvim_win_set_cursor(0, { line, col })
	vim.cmd("normal! zz")
	return true
end

function M.open_files(paths, opts)
	opts = opts or {}
	local opened = false

	for index, path in ipairs(paths or {}) do
		path = normalize_path(path)
		if path ~= "" and vim.fn.filereadable(path) == 1 then
			if not opened then
				opened = M.open_location(path, opts.line, opts.col, opts)
			else
				vim.cmd("badd " .. vim.fn.fnameescape(path))
			end
		end
	end

	if not opened and not opts.silent then
		vim.notify("UCore open: no readable files in request", vim.log.levels.WARN)
	end

	return opened
end

return M
