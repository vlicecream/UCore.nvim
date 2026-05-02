local actions = require("ucore.commands.actions")
local config = require("ucore.config")

local M = {}

local function normalize_subcommand(args)
	local sub = (args.args or ""):match("^%s*(%S+)")
	return sub and sub:lower() or "smart_entry"
end

local function command_tail(args)
	return (args.args or ""):match("^%s*%S+%s*(.-)%s*$") or ""
end

local function split_first(text)
	local head, tail = (text or ""):match("^%s*(%S*)%s*(.-)%s*$")
	if head == "" then
		return "help", ""
	end

	return head:lower(), tail or ""
end

local function dispatch_debug(tail)
	local sub, rest = split_first(tail)

	local handlers = {
		logs = actions.logs,
		engine = actions.engine,
		["engine-refresh"] = actions.engine_refresh,
		enginerefresh = actions.engine_refresh,
		open = actions.open_project,
		register = actions.register_project,
		projects = actions.projects,
		modules = actions.modules,
		assets = actions.assets,
		clangd = actions.clangd_status,
		["generate-db"] = actions.generate_compile_commands,
		generatedb = actions.generate_compile_commands,
		["search-symbols"] = function()
			actions.search_symbols(rest)
		end,
		searchsymbols = function()
			actions.search_symbols(rest)
		end,
		status = actions.status,
		["rpc-status"] = actions.rpc_status,
		rpcstatus = actions.rpc_status,
		setup = actions.setup,
		refresh = actions.refresh,
		start = actions.start,
		stop = actions.stop,
		restart = actions.restart,
		maps = actions.maps,
		editing = actions.editing_debug,
		help = actions.debug_help,
		complete = actions.complete,
		["goto"] = actions.goto_definition,
	}

	local handler = handlers[sub]
	if not handler then
		vim.notify("Unknown UCore debug command: " .. sub, vim.log.levels.ERROR)
		return actions.debug_help()
	end

	handler()
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
		tree = actions.explorer,
		files = actions.explorer,
		globalfind = function()
			actions.global_find(tail)
		end,
		diagnostics = function()
			require("ucore.diagnostics").dispatch(tail)
		end,
		["goto"] = function()
			actions.goto(tail)
		end,
		debug = function()
			dispatch_debug(tail)
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
	local function complete()
		require("ucore.completion").complete()
	end

	local completion_config = config.values.completion or {}
	local keymap = completion_config.keymap

	if completion_config.enable ~= false and keymap and keymap ~= "" then
		vim.keymap.set("i", keymap, complete, {
			desc = "UCore complete",
		})
	end

	vim.api.nvim_create_user_command("UCore", M.dispatch, {
		nargs = "*",
		complete = function(arglead, cmdline, cursorpos)
			local user_items = {
				"boot",
				"build",
				"build-cancel",
				"editor",
				"explorer",
				"tree",
				"files",
				"globalfind",
				"diagnostics",
				"goto",
				"debug",
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

			local debug_items = {
				"logs",
				"engine",
				"engine-refresh",
				"open",
				"register",
				"projects",
				"modules",
				"assets",
				"clangd",
				"generate-db",
				"search-symbols",
				"status",
				"rpc-status",
				"setup",
				"refresh",
				"start",
				"stop",
				"restart",
				"maps",
				"complete",
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
			local in_debug = first and first:lower() == "debug"
			local in_goto = first and first:lower() == "goto"

			local items
			if in_debug then
				items = debug_items
			elseif in_goto then
				items = goto_items
			elseif first and first:lower() == "diagnostics" then
				items = diagnostics_items
			else
				items = user_items
			end

			local needle = (arglead or ""):lower()

			if in_debug and (tail:lower() == "debug" or tail:lower():match("^debug%s*$")) then
				needle = ""
			end

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
