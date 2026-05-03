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
		build = function()
			actions.build(tail)
		end,
		["build-cancel"] = actions.build_cancel,
		buildcancel = actions.build_cancel,
		editor = function()
			actions.editor(tail)
		end,
		explorer = actions.explorer,
		find = function()
			actions.find(tail)
		end,
		diagnostics = function()
			require("ucore.diagnostics").dispatch(tail)
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
	vim.api.nvim_create_user_command("UCore", M.dispatch, {
		nargs = "*",
		complete = function(arglead, cmdline, cursorpos)
			local user_items = {
				"boot",
				"build",
				"build-cancel",
				"editor",
				"explorer",
				"find",
				"diagnostics",
				"goto",
				"help",
			}

			local diagnostics_items = {
				"refresh",
				"clear",
				"action",
				"fix",
				"qflist",
				"toggle",
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
			local in_goto = first and first:lower() == "goto"

			local items
			if in_goto then
				items = goto_items
			elseif first and first:lower() == "diagnostics" then
				items = diagnostics_items
			else
				items = user_items
			end

			local needle = (arglead or ""):lower()

			if first and first:lower() == "diagnostics" and tail:lower():match("^diagnostics%s*$") then
				needle = ""
			end

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
