local config = require("ucore.config")
local log = require("ucore.log")
local status = require("ucore.status")

local M = {}

local default_phase_order = {
	"discovery",
	"db_prepare",
	"analysis",
	"db_write",
	"text_write",
	"asset_index",
	"finalizing",
}

local default_phases = {
	discovery = { label = "Discovery", weight = 0.05 },
	db_prepare = { label = "DB Prepare", weight = 0.05 },
	analysis = { label = "Analysis", weight = 0.45 },
	db_write = { label = "DB Write", weight = 0.20 },
	text_write = { label = "Text DB Write", weight = 0.05 },
	asset_index = { label = "Asset Index", weight = 0.10 },
	finalizing = { label = "Finalizing", weight = 0.10 },
}

local sessions = {}
local start_order = {}
local next_local_id = 0

local function clone_default_phases()
	return vim.deepcopy(default_phases)
end

local function clone_default_phase_order()
	return vim.deepcopy(default_phase_order)
end

local function new_session(id, next_title, opts)
	opts = opts or {}
	return {
		id = id,
		title = next_title or "UCore refresh",
		target_kind = opts.target_kind or "project",
		visible = opts.silent ~= true,
		auto_finish = opts.auto_finish ~= false,
		phases = clone_default_phases(),
		phase_order = clone_default_phase_order(),
		stage_progress = {},
		title_rendered = {},
		title_done = {},
		last_percent = -1,
		last_stage = nil,
		last_detail = nil,
		current_display_title = nil,
		active_titles = {},
		active = true,
	}
end

local function clamp(value, min, max)
	if value < min then
		return min
	end
	if value > max then
		return max
	end
	return value
end

local function normalize_phase(phase)
	if type(phase) ~= "table" then
		return nil
	end

	return {
		name = phase.name or phase[1],
		label = phase.label or phase[2],
		weight = phase.weight or phase[3],
	}
end

local function normalize_plan(plan)
	if type(plan) ~= "table" then
		return {}
	end

	return plan.phases or plan[2] or {}
end

local function normalize_event(event)
	if type(event) ~= "table" then
		return {}
	end

	return {
		stage = event.stage or event[2],
		current = tonumber(event.current or event[3]) or 0,
		total = tonumber(event.total or event[4]) or 0,
		message = event.message or event[5],
	}
end

local function normalize_detail(message)
	return vim.trim(tostring(message or ""))
end

local function apply_target_kind(session, target_kind)
	if target_kind == "project" or target_kind == "engine" then
		session.target_kind = target_kind
	end
end

local function target_prefix(session)
	if session.target_kind == "engine" then
		return "UCore Engine"
	end
	return "UCore Project"
end

local function stage_label(session, stage)
	local phase = session.phases[stage]
	if phase and phase.label and phase.label ~= "" then
		return phase.label
	end
	if type(stage) ~= "string" or stage == "" then
		return nil
	end

	local words = {}
	for part in stage:gmatch("[^_]+") do
		table.insert(words, part:sub(1, 1):upper() .. part:sub(2))
	end
	return #words > 0 and table.concat(words, " ") or nil
end

local function asset_stage_label(detail)
	local normalized = normalize_detail(detail):lower()
	if normalized:find("persist", 1, true)
		or normalized:find("ready", 1, true)
		or normalized:find("runtime", 1, true)
	then
		return "Asset Persist"
	end
	return "Asset Scan"
end

local function display_title_for_event(session, event, detail)
	local prefix = target_prefix(session)
	local stage = event.stage

	if stage == "discovery" then
		return prefix .. " Discovery"
	end
	if stage == "db_prepare" then
		return prefix .. " DB Prepare"
	end
	if stage == "analysis" then
		return prefix .. " Analysis"
	end
	if stage == "db_write" then
		return prefix .. " DB Write"
	end
	if stage == "finalizing" then
		return prefix .. " Finalize"
	end
	if stage == "asset_index" then
		return prefix .. " " .. asset_stage_label(detail)
	end

	return prefix .. " " .. (stage_label(session, stage) or "Progress")
end

local function monotonic_event(session, event)
	local stage = event.stage
	if type(stage) ~= "string" or stage == "" then
		return event
	end

	local previous = session.stage_progress[stage]
	if previous then
		if event.total > 0 and previous.total > 0 and event.total < previous.total then
			event.total = previous.total
		end
		if event.current < previous.current then
			event.current = previous.current
		end
	end

	session.stage_progress[stage] = {
		current = event.current,
		total = event.total,
	}
	return event
end

local function overall_percent(session, event)
	local stage = event.stage
	if stage == "complete" then
		return 100
	end

	local phase = session.phases[stage]
	if not phase then
		local current = tonumber(event.current) or 0
		local total = tonumber(event.total) or 100
		return clamp(math.floor((current / math.max(total, 1)) * 100), 0, 100)
	end

	local before = 0
	for _, name in ipairs(session.phase_order) do
		if name == stage then
			break
		end
		before = before + ((session.phases[name] and session.phases[name].weight) or 0)
	end

	local current = tonumber(event.current) or 0
	local total = tonumber(event.total) or 100
	local local_ratio = clamp(current / math.max(total, 1), 0, 1)
	local percent = (before + local_ratio * phase.weight) * 100
	return clamp(math.floor(percent), session.last_percent, 100)
end

local function stage_percent(event)
	local current = tonumber(event.current) or 0
	local total = tonumber(event.total) or 100
	return clamp(math.floor((current / math.max(total, 1)) * 100), 0, 100)
end

local function finish_active_titles(session, message)
	if not session.visible then
		return
	end

	local titles = {}
	for name, active_flag in pairs(session.active_titles) do
		if active_flag then
			table.insert(titles, name)
		end
	end
	if #titles == 0 and session.current_display_title then
		table.insert(titles, session.current_display_title)
	end

	table.sort(titles)
	for _, finish_title in ipairs(titles) do
		status.progress_finish(finish_title, string.format("%s 100%%", finish_title))
	end

	session.active_titles = {}
end

local function session_by_msgid(msgid)
	if msgid ~= nil and sessions[msgid] then
		return sessions[msgid]
	end

	if #start_order > 0 then
		local fallback_id = table.remove(start_order, 1)
		local fallback_session = sessions[fallback_id]
		if fallback_session then
			sessions[msgid or fallback_id] = fallback_session
			if msgid ~= nil then
				sessions[fallback_id] = nil
				fallback_session.id = msgid
			end
			return fallback_session
		end
	end

	return nil
end

local function register_start(next_title, opts, msgid)
	next_local_id = next_local_id + 1
	local id = msgid or ("local:" .. tostring(next_local_id))
	local session = new_session(id, next_title, opts)
	sessions[id] = session
	table.insert(start_order, id)
	return session
end

local function cleanup_session(id)
	sessions[id] = nil
	for index = #start_order, 1, -1 do
		if start_order[index] == id then
			table.remove(start_order, index)
		end
	end
end

function M.start(next_title, opts, msgid)
	local session = register_start(next_title, opts, msgid)
	log.write_progress("progress-start", {
		msgid = msgid,
		title = session.title,
		target_kind = session.target_kind,
		visible = session.visible,
	})
	if session.visible then
		local initial_message = opts and opts.detail and opts.detail ~= ""
				and tostring(opts.detail)
			or string.format("%s 0%%", session.title)
		status.progress(session.title, initial_message)
		session.title_rendered[session.title] = initial_message
		session.active_titles[session.title] = true
		session.current_display_title = session.title
	end
end

function M.finish(message, msgid)
	local session = session_by_msgid(msgid)
	if not session then
		log.write_progress("progress-finish-miss", {
			msgid = msgid,
			message = message,
		})
		return
	end
	log.write_progress("progress-finish", {
		msgid = msgid,
		title = session.title,
		target_kind = session.target_kind,
		current_display_title = session.current_display_title,
		message = message,
	})

	session.active = false
	session.last_percent = 100
	session.last_stage = "complete"
	session.last_detail = nil
	local finish_title = session.current_display_title or session.title
	local finish_message = message or string.format("%s 100%%", finish_title)
	finish_active_titles(session, finish_message)
	cleanup_session(session.id)
end

function M.fail(message, msgid)
	local session = session_by_msgid(msgid)
	if not session then
		return
	end

	session.active = false
	session.last_stage = "failed"
	session.last_detail = nil
	if session.visible then
		status.progress_fail(session.current_display_title or session.title, message or string.format("%s failed", session.title))
	end
	cleanup_session(session.id)
end

function M.handle_plan(plan, msgid, target_kind)
	local session = session_by_msgid(msgid)
	if not session or not session.active then
		log.write_progress("progress-plan-miss", {
			msgid = msgid,
			target_kind = target_kind,
		})
		return
	end
	apply_target_kind(session, target_kind)
	log.write_progress("progress-plan-ui", {
		msgid = msgid,
		target_kind = session.target_kind,
		title = session.title,
	})

	local items = normalize_plan(plan)
	if type(items) ~= "table" then
		return
	end

	session.phases = {}
	session.phase_order = {}
	session.stage_progress = {}
	session.last_percent = -1
	session.last_stage = nil
	session.last_detail = nil

	for _, raw_phase in ipairs(items) do
		local phase = normalize_phase(raw_phase)
		local name = phase and phase.name
		if name then
			table.insert(session.phase_order, name)
			session.phases[name] = {
				label = phase.label or name,
				weight = tonumber(phase.weight) or 0,
			}
		end
	end

	if vim.tbl_isempty(session.phases) then
		session.phases = clone_default_phases()
		session.phase_order = clone_default_phase_order()
	end
end

function M.handle_progress(event, msgid, target_kind)
	local session = session_by_msgid(msgid)
	if not session or not session.active then
		log.write_progress("progress-event-miss", {
			msgid = msgid,
			target_kind = target_kind,
			stage = type(event) == "table" and (event.stage or event[2]) or nil,
		})
		return
	end
	apply_target_kind(session, target_kind)

	event = monotonic_event(session, normalize_event(event))
	if event.stage == "complete" then
		if session.auto_finish then
			return M.finish(nil, session.id)
		end
		return M.finish(normalize_detail(event.message), session.id)
	end

	local detail = normalize_detail(event.message)
	local display_title = display_title_for_event(session, event, detail)
	local percent = stage_percent(event)
	local overall = overall_percent(session, event)
	local rendered = string.format("%s %d%%", display_title, percent)
	if detail ~= "" and percent <= 0 then
		rendered = detail
	elseif detail ~= "" and percent < 100 and rendered == string.format("%s 0%%", display_title) then
		rendered = string.format("%s - %s", display_title, detail)
	end

	local same_render = rendered == session.title_rendered[display_title]
	if same_render and percent < 100 then
		return
	end

	session.title_rendered[display_title] = rendered
	session.last_percent = overall
	session.last_stage = event.stage
	session.last_detail = rendered
	session.active_titles[display_title] = true
	session.current_display_title = display_title

	log.write_progress("progress-ui", {
		msgid = msgid,
		title = session.title,
		stage = event.stage,
		current = event.current,
		total = event.total,
		overall = overall,
		target_kind = session.target_kind,
		display_title = display_title,
		detail = detail,
	})

	if percent >= 100 then
		session.active_titles[display_title] = nil
		session.title_done[display_title] = true
		return status.progress_finish(display_title, string.format("%s 100%%", display_title))
	end

	if session.visible then
		status.progress(display_title, rendered)
	end
end

return M
