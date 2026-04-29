local actions = require("ucore.commands.actions")
local config = require("ucore.config")

local M = {}

-- Convert subcommands to lowercase for case-insensitive dispatch.
-- 把子命令转成小写，从而支持大小写不敏感。
local function normalize_subcommand(args)
	local sub = (args.args or ""):match("^%s*(%S+)")
	return sub and sub:lower() or "smart_entry"
end

-- Return the rest of the command line after the subcommand.
-- 返回子命令后面的剩余参数。
local function command_tail(args)
	return (args.args or ""):match("^%s*%S+%s*(.-)%s*$") or ""
end

-- Return the first token and remaining tail from a command fragment.
-- 从命令片段中取出第一个 token 和剩余参数。
local function split_first(text)
	local head, tail = (text or ""):match("^%s*(%S*)%s*(.-)%s*$")
	if head == "" then
		return "help", ""
	end

	return head:lower(), tail or ""
end

-- Dispatch debug-only subcommands.
-- 分发仅用于调试的子命令。
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
		["p4-changes"] = actions.pending_changelists,
		p4changes = actions.pending_changelists,
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
		help = actions.debug_help,
		complete = actions.complete,
		goto = actions.goto_definition,
		references = actions.references,
		vcs = actions.vcs_debug,
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
    find = function()
      actions.find(tail)
    end,
    goto = actions.goto_definition,
		references = actions.references,
		changes = actions.changes,
		checkout = actions.checkout,
		commit = actions.commit,
		changelists = actions.changelists,
		vcs = function()
			actions.vcs_dispatch(tail)
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

-- Register a single user command with subcommands.
-- 注册一个带子命令的用户命令。
function M.register()
	-- Provide a simple manual insert-mode completion mapping.
	-- 提供一个简单的插入模式手动补全快捷键。
	local function complete()
		require("ucore.completion").complete()
	end

	local completion_config = config.values.completion or {}
	local keymap = completion_config.keymap

	if completion_config.enable ~= false and keymap and keymap ~= "" then
		-- Manual completion mapping, configurable by users.
		-- 用户可配置的手动补全快捷键。
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
				"vcs",
				"vcs dashboard",
				"vcs changes",
				"vcs changelists",
				"vcs checkout",
				"vcs commit",
				"changes",
				"changelists",
				"checkout",
				"commit",
				"editor",
				"find",
				"goto",
				"references",
				"debug",
				"help",
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
				"p4-changes",
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
				"references",
				"vcs",
				"help",
			}

			local line = cmdline or ""
			local before_cursor = line:sub(1, (cursorpos or (#line + 1)) - 1)
			local tail = before_cursor:match("^%s*UCore%s*(.-)%s*$") or ""
			local first = tail:match("^(%S+)")
			local in_debug = first and first:lower() == "debug"

			local items = in_debug and debug_items or user_items
			local needle = (arglead or ""):lower()

			if in_debug and (tail:lower() == "debug" or tail:lower():match("^debug%s*$")) then
				needle = ""
			end

			return vim.tbl_filter(function(item)
				return item:find(needle, 1, true) == 1
			end, items)
		end,
	})
end

return M
