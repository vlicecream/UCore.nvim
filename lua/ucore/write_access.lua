local M = {}

local function normalize(path)
	return tostring(path or ""):gsub("\\", "/")
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

	if vim.fn.filereadable(path) ~= 1 then
		return true, nil
	end

	if vim.fn.filewritable(path) == 1 then
		return true, nil
	end

	local provider = M.detect_provider(path)
	return prompt_write_access(path, provider, opts)
end

return M
