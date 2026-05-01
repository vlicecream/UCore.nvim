local config = require("ucore.config")

local M = {}

local function load()
	local ok, npairs = pcall(require, "nvim-autopairs")
	if not ok then
		return nil
	end
	return npairs
end

local function ensure_setup(npairs)
	if npairs.config and npairs.config.rules then
		return
	end

	npairs.setup({
		enable_check_bracket_line = false,
		fast_wrap = {},
		map_cr = config.values.autopairs.map_cr ~= false,
		check_ts = config.values.autopairs.check_ts ~= false,
	})
end

local function mark(rule)
	rule._ucore = true
	return rule
end

local function remove_ucore_rules(npairs)
	if not npairs.config or not npairs.config.rules then
		return
	end

	local rules = {}
	for _, rule in ipairs(npairs.config.rules) do
		if not rule._ucore then
			table.insert(rules, rule)
		end
	end
	npairs.config.rules = rules
end

local function add_ucore_rules(npairs)
	local Rule = require("nvim-autopairs.rule")
	local cond = require("nvim-autopairs.conds")

	remove_ucore_rules(npairs)

	-- {} multiline expansion with correct indentation.
	-- {} 回车展开 + 正确缩进。
	npairs.add_rules({
		mark(Rule("(", ")", { "cpp", "unreal_cpp" })
			:with_cr(cond.done())),
		mark(Rule("{", "}", { "cpp", "unreal_cpp" })
			:with_cr(cond.done())),
	})

	-- UE macro auto-complete: UFUNCTION( → UFUNCTION()
	-- UE 宏自动补全：UFUNCTION( → UFUNCTION()
	npairs.add_rules({
		mark(Rule("UFUNCTION", ")", "cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true)),
		mark(Rule("UPROPERTY", ")", "cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true)),
		mark(Rule("UFUNCTION", ")", "unreal_cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true)),
		mark(Rule("UPROPERTY", ")", "unreal_cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true)),
	})
end

function M.apply()
	if config.values.autopairs.enable == false then
		return false
	end

	local npairs = load()
	if not npairs then
		return false
	end

	ensure_setup(npairs)
	add_ucore_rules(npairs)
	if config.values.autopairs.map_cr ~= false and vim.fn.maparg("<CR>", "i") == "" then
		npairs.map_cr()
	end
	return true
end

function M.setup()
	if config.values.autopairs.enable == false then
		return
	end

	local npairs = load()
	if npairs then
		M.apply()
	end

	local group = vim.api.nvim_create_augroup("UCoreAutopairs", { clear = true })
	vim.api.nvim_create_autocmd({ "InsertEnter", "FileType" }, {
		group = group,
		pattern = "*",
		callback = function()
			vim.defer_fn(M.apply, 50)
			vim.defer_fn(M.apply, 250)
		end,
	})
	vim.api.nvim_create_autocmd("User", {
		group = group,
		pattern = { "LazyDone", "VeryLazy" },
		callback = function()
			vim.defer_fn(M.apply, 50)
			vim.defer_fn(M.apply, 250)
		end,
	})
end

return M
