local M = {}

local function normalize(path)
	return tostring(path or ""):gsub("\\", "/")
end

local function readable(path)
	return vim.fn.filereadable(path) == 1
end

local function writable(path)
	return vim.fn.filewritable(path) == 1
end

function M.detect_provider(path)
	local ok_uvcs, uvcs = pcall(require, "uvcs")
	if not ok_uvcs or type(uvcs) ~= "table" or type(uvcs.detect_for_path) ~= "function" then
		return nil
	end

	local ok_provider, provider = pcall(uvcs.detect_for_path, path)
	if not ok_provider or type(provider) ~= "table" then
		return nil
	end

	return provider
end

function M.make_writable(path, provider)
	if provider and type(provider.make_writable) == "function" then
		provider.make_writable(path)
	else
		if vim.fn.has("win32") == 1 then
			vim.fn.system({ "attrib", "-R", path })
		else
			vim.fn.system({ "chmod", "u+w", path })
		end
	end

	return vim.fn.filewritable(path) == 1
end

local function batch_summary(paths)
	local preview = {}
	for index, path in ipairs(paths or {}) do
		if index > 5 then
			break
		end
		table.insert(preview, vim.fn.fnamemodify(path, ":t"))
	end

	local suffix = (#paths > #preview) and string.format("\n... and %d more", #paths - #preview) or ""
	return table.concat(preview, "\n") .. suffix
end

local function prompt_write_access(path, provider, opts)
	opts = opts or {}
	local has_checkout = provider and type(provider.checkout) == "function"
	local already_opened = has_checkout and provider.is_opened and provider.is_opened(path) or false
	local fname = vim.fn.fnamemodify(path, ":t")
	local action = tostring(opts.action or "modify file")

	local buttons
	if has_checkout then
		buttons = "&P4 checkout/edit\n&Make writable only\n&Cancel"
	else
		buttons = "&Make writable only\n&Cancel"
	end

	local choice = vim.fn.confirm(
		"UCore: target file is read-only\n\n" .. fname .. "\n\nChoose how to continue " .. action .. ":",
		buttons,
		1,
		"Warning"
	)

	if has_checkout then
		if choice == 1 then
			if already_opened then
				if M.make_writable(path, provider) then
					return true, nil
				end
				return false, "target file is still read-only after making writable: " .. path
			end

			local ok_checkout, checkout_err = provider.checkout(path)
			if not ok_checkout then
				return false, checkout_err or ("p4 edit failed for target file: " .. path)
			end

			if M.make_writable(path, provider) then
				return true, nil
			end

			return false, "target file is still read-only after checkout: " .. path
		end

		if choice == 2 then
			if M.make_writable(path, provider) then
				return true, nil
			end
			return false, "failed to make target file writable: " .. path
		end

		return false, action .. " cancelled"
	end

	if choice == 1 then
		if M.make_writable(path, provider) then
			return true, nil
		end
		return false, "failed to make target file writable: " .. path
	end

	return false, action .. " cancelled"
end

function M.ensure_writable(path, opts)
	path = normalize(path)
	if path == "" then
		return false, "invalid target path"
	end

	if not readable(path) then
		return true, nil
	end

	if writable(path) then
		return true, nil
	end

	local provider = M.detect_provider(path)
	return prompt_write_access(path, provider, opts)
end

function M.ensure_writable_many(paths, opts)
	opts = opts or {}
	local action = tostring(opts.action or "modify files")
	local targets = {}

	for _, path in ipairs(paths or {}) do
		path = normalize(path)
		if path ~= "" and readable(path) and not writable(path) then
			table.insert(targets, {
				path = path,
				provider = M.detect_provider(path),
			})
		end
	end

	if #targets == 0 then
		return true, nil
	end

	if #targets == 1 then
		return M.ensure_writable(targets[1].path, opts)
	end

	local checkout_available = true
	for _, target in ipairs(targets) do
		if type(target.provider) ~= "table" or type(target.provider.checkout) ~= "function" then
			checkout_available = false
			break
		end
	end

	local buttons
	if checkout_available then
		buttons = "&P4 checkout all\n&Make all writable\n&Cancel"
	else
		buttons = "&Make all writable\n&Cancel"
	end

	local choice = vim.fn.confirm(
		string.format(
			"UCore: %d target files are read-only\n\n%s\n\nChoose how to continue %s:",
			#targets,
			batch_summary(vim.tbl_map(function(item)
				return item.path
			end, targets)),
			action
		),
		buttons,
		1,
		"Warning"
	)

	if checkout_available then
		if choice == 1 then
			for _, target in ipairs(targets) do
				local provider = target.provider
				local already_opened = provider.is_opened and provider.is_opened(target.path) or false
				if not already_opened then
					local ok_checkout, checkout_err = provider.checkout(target.path)
					if not ok_checkout then
						return false, checkout_err or ("p4 edit failed for target file: " .. target.path)
					end
				end

				if not M.make_writable(target.path, provider) then
					return false, "target file is still read-only after checkout: " .. target.path
				end
			end

			return true, nil
		end

		if choice == 2 then
			for _, target in ipairs(targets) do
				if not M.make_writable(target.path, target.provider) then
					return false, "failed to make target file writable: " .. target.path
				end
			end

			return true, nil
		end

		return false, action .. " cancelled"
	end

	if choice == 1 then
		for _, target in ipairs(targets) do
			if not M.make_writable(target.path, target.provider) then
				return false, "failed to make target file writable: " .. target.path
			end
		end

		return true, nil
	end

	return false, action .. " cancelled"
end

return M
