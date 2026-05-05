local actions = require("ucore.commands.actions")

local M = {}

local function normalize_subcommand(args)
	local sub = (args.args or ""):match("^%s*(%S+)")
	return sub and sub:lower() or "smart_entry"
end

local function command_tail(args)
	return (args.args or ""):match("^%s*%S+%s*(.-)%s*$") or ""
end

function M.dispatch(args)
	local sub = normalize_subcommand(args)
	local tail = command_tail(args)

	local handlers = {
		smart_entry = actions.smart_entry,
		dashboard = actions.dashboard,
		boot = actions.boot,
		explorer = actions.explorer,
		find = function()
			actions.find(tail)
		end,
		["goto"] = function()
			actions.goto(tail)
		end,
		help = actions.help,
	}

	local handler = handlers[sub]
	if not handler then
		vim.notify("Unknown UCore command: " .. sub, vim.log.levels.ERROR)
		return actions.help()
	end

	handler()
end

function M.register()
	pcall(vim.api.nvim_del_user_command, "UCore")

	vim.api.nvim_create_user_command("UCore", M.dispatch, {
		nargs = "*",
		complete = function(arglead, cmdline, cursorpos)
			local user_items = {
				"boot",
				"explorer",
				"find",
				"goto",
				"help",
			}

			local goto_items = {
				"definition",
				"declaration",
				"implementation",
				"references",
				"source",
				"help",
			}

			local line = cmdline or ""
			local before_cursor = line:sub(1, (cursorpos or (#line + 1)) - 1)
			local tail = before_cursor:match("^%s*UCore%s*(.-)%s*$") or ""
			local first = tail:match("^(%S+)")
			local first_lower = first and first:lower() or nil
			local in_goto = first_lower == "goto"

			local items
			if in_goto then
				items = goto_items
			else
				items = user_items
			end

			local needle = (arglead or ""):lower()

			if in_goto and (tail:lower() == "goto" or tail:lower():match("^goto%s*$")) then
				needle = ""
			end

			return vim.tbl_filter(function(item)
				return item:find(needle, 1, true) == 1
			end, items)
		end,
	})
end

return M
