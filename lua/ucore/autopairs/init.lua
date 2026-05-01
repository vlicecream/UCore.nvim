local config = require("ucore.config")

local M = {}

function M.setup()
	if config.values.autopairs.enable == false then
		return
	end

	local ok, npairs = pcall(require, "nvim-autopairs")
	if not ok then
		return
	end

	npairs.setup({
		enable_check_bracket_line = false,
		fast_wrap = {},
	})

	local Rule = require("nvim-autopairs.rule")
	local cond = require("nvim-autopairs.conds")

	-- {} multiline expansion with correct indentation.
	-- {} 回车展开 + 正确缩进。
	local RuleN = require("nvim-autopairs.rule")
	local cond = require("nvim-autopairs.conds")

	npairs.add_rules({
		RuleN("(", ")", { "cpp", "unreal_cpp" })
			:with_cr(cond.done())
			:with_indent(cond.done()),
		RuleN("{", "}", { "cpp", "unreal_cpp" })
			:with_cr(cond.done())
			:with_indent(cond.done()),
	})

	-- UE macro auto-complete: UFUNCTION( → UFUNCTION()
	-- UE 宏自动补全：UFUNCTION( → UFUNCTION()
	npairs.add_rules({
		Rule("UFUNCTION", ")", "cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true),
		Rule("UPROPERTY", ")", "cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true),
		Rule("UFUNCTION", ")", "unreal_cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true),
		Rule("UPROPERTY", ")", "unreal_cpp")
			:with_pair(cond.not_before_text("*/"))
			:with_cr(true),
	})
end

return M
