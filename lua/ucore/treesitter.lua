local M = {}

local parser_name = "unreal_cpp"
local parser_repo = "https://github.com/vlicecream/UTreeSitter"

local function is_unreal_project(path)
	local markers = vim.fs.find(function(name)
		return name:match("%.uproject$") or name:match("%.uplugin$")
	end, {
		path = vim.fs.dirname(path),
		upward = true,
		type = "file",
		limit = 1,
	})
	return #markers > 0
end

local function augment_parsers(parsers)
	parsers[parser_name] = vim.tbl_deep_extend("force", parsers[parser_name] or {}, {
		install_info = {
			url = parser_repo,
			files = { "src/parser.c", "src/scanner.c" },
			queries = "queries/unreal_cpp",
			generate_requires_npm = false,
			requires_generate_from_grammar = false,
		},
		filetype = parser_name,
		maintainers = { "@vlicecream" },
		tier = 2,
	})

	return parsers
end

local function stock_parsers_path()
	return vim.fn.stdpath("data") .. "/lazy/nvim-treesitter/lua/nvim-treesitter/parsers.lua"
end

local function build_parsers()
	local module_path = stock_parsers_path()
	if not vim.uv.fs_stat(module_path) then
		return nil
	end

	return augment_parsers(dofile(module_path))
end

local function register_parser()
	local ok, parsers = pcall(require, "nvim-treesitter.parsers")
	if ok and type(parsers) == "table" then
		augment_parsers(parsers)
		package.loaded["nvim-treesitter.parsers"] = parsers
	end
end

local function apply_highlight_links()
	local hl = vim.api.nvim_set_hl

	local styles = {
		["@keyword.directive.unreal_cpp"] = { fg = "#4FC1FF", bold = true },
		["@keyword.function.unreal_cpp"] = { fg = "#C792EA", bold = true },

		["@type.unreal_cpp"] = { fg = "#C586C0" },
		["@type.enum.unreal_cpp"] = { fg = "#C586C0", bold = true },
		["@type.builtin.unreal_cpp"] = { fg = "#4FC1FF" },
		["@type.qualifier.unreal_cpp"] = { fg = "#4FC1FF", italic = true },

		["@function.unreal_cpp"] = { fg = "#4EC9B0" },
		["@function.method.unreal_cpp"] = { fg = "#4EC9B0", italic = true },
		["@function.macro.unreal_cpp"] = { fg = "#D19A66", bold = true },
		["@function.macro.delegate.unreal_cpp"] = { fg = "#D19A66", bold = true, italic = true },

		["@property.unreal_cpp"] = { fg = "#61AFEF" },
		["@variable.unreal_cpp"] = { fg = "#E5C07B" },
		["@variable.builtin.unreal_cpp"] = { fg = "#56B6C2" },
		["@parameter.unreal_cpp"] = { fg = "#FFD866" },

		["@string.unreal_cpp"] = { fg = "#CE9178" },
		["@string.special.unreal_cpp"] = { fg = "#D7BA7D" },
		["@number.unreal_cpp"] = { fg = "#F78C6C" },

		["@constant.unreal_cpp"] = { fg = "#C678DD" },
		["@constant.enum.unreal_cpp"] = { fg = "#F78C6C", bold = true },
		["@constant.builtin.unreal_cpp"] = { fg = "#56B6C2", bold = true },
		["@macro.unreal_cpp"] = { fg = "#D19A66", bold = true },
	}

	for target, style in pairs(styles) do
		hl(0, target, style)
	end
end

local function unreal_filetype(path)
	if is_unreal_project(path) then
		return parser_name
	end
end

function M.setup()
	package.preload["nvim-treesitter.parsers"] = function()
		if type(package.loaded["nvim-treesitter.parsers"]) == "table" then
			return package.loaded["nvim-treesitter.parsers"]
		end

		local parsers = build_parsers()
		if not parsers then
			error("nvim-treesitter.parsers not available")
		end

		package.loaded["nvim-treesitter.parsers"] = parsers
		return parsers
	end

	register_parser()
	vim.schedule(register_parser)

	apply_highlight_links()
	vim.api.nvim_create_autocmd("ColorScheme", {
		group = vim.api.nvim_create_augroup("UCoreUnrealHighlights", { clear = true }),
		callback = apply_highlight_links,
	})

	vim.filetype.add({
		extension = {
			cpp = unreal_filetype,
			h = unreal_filetype,
			hpp = unreal_filetype,
			hh = unreal_filetype,
			cc = unreal_filetype,
			cxx = unreal_filetype,
			inl = unreal_filetype,
		},
	})
end

return M
