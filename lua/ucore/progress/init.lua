local config = require("ucore.config")
local log = require("ucore.log")
local status = require("ucore.status")

local M = {}

-- Default refresh phase weights used when the Rust plan has not arrived yet.
-- 当 Rust 阶段计划还没到时使用的默认整体进度权重。
local default_phase_order = {
	"discovery",
	"db_prepare",
	"analysis",
	"db_write",
	"asset_index",
	"finalizing",
}

local default_phases = {
	discovery = { label = "Discovery", weight = 0.05 },
	db_prepare = { label = "DB Prepare", weight = 0.05 },
	analysis = { label = "Analysis", weight = 0.45 },
	db_write = { label = "DB Write", weight = 0.25 },
	asset_index = { label = "Asset Index", weight = 0.10 },
	finalizing = { label = "Finalizing", weight = 0.10 },
}

local phases = {}
local phase_order = {}
local last_percent = -1
local last_stage = nil
local last_detail = nil
local last_tail = nil
local active = false
local title = "UCore refresh"
local visible = true
local stage_progress = {}
local target_kind = "project"
local current_display_title = nil
local auto_finish = true

-- Load the built-in overall progress plan.
-- 加载内置的整体进度计划。
local function load_default_plan()
	phases = vim.deepcopy(default_phases)
	phase_order = vim.deepcopy(default_phase_order)
end

-- Reset progress state for a new refresh run.
-- 为新的 refresh 运行重置进度状态。
local function reset()
	load_default_plan()
	last_percent = -1
	last_stage = nil
	last_detail = nil
	last_tail = nil
	stage_progress = {}
	active = true
	current_display_title = nil
	auto_finish = true
end

-- Start a new visible progress run with a user-facing title.
-- 使用面向用户的标题开始一次新的进度展示。
function M.start(next_title, opts)
	opts = opts or {}
	title = next_title or "UCore refresh"
	target_kind = opts.target_kind or "project"
	auto_finish = opts.auto_finish ~= false
	visible = opts.silent ~= true
	reset()
	auto_finish = opts.auto_finish ~= false
	if visible then
		status.progress(title, string.format("%s 0%%", title))
	end
end

-- Finish the visible progress run and let it disappear shortly after.
-- 完成当前进度展示，并在短暂显示后自动消失。
function M.finish(message)
	if not active then
		return
	end

	active = false
	last_percent = 100
	last_stage = "complete"
	local finish_title = current_display_title or title
	local finish_message = message or string.format("%s 100%%", finish_title)
	last_detail = nil
	last_tail = nil
	if visible then
		status.progress_finish(finish_title, finish_message)
	end
end

-- Mark the current progress run as failed.
-- 标记当前进度展示失败。
function M.fail(message)
	active = false
	last_stage = "failed"
	last_detail = nil
	last_tail = nil
	if visible then
		status.progress_fail(current_display_title or title, message or string.format("%s failed", title))
	end
end

-- Clamp one numeric value into a safe range.
-- 把数字限制到安全范围内。
local function clamp(value, min, max)
	if value < min then
		return min
	end

	if value > max then
		return max
	end

	return value
end

-- Normalize a msgpack-decoded phase. rmp-serde may encode Rust structs as
-- either maps or arrays, depending on serializer settings.
-- 规范化 msgpack 解码后的阶段；Rust struct 可能被编码成 map，也可能是数组。
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

-- Normalize a msgpack-decoded progress plan.
-- 规范化 msgpack 解码后的 progress plan。
local function normalize_plan(plan)
	if type(plan) ~= "table" then
		return {}
	end

	return plan.phases or plan[2] or {}
end

-- Normalize a msgpack-decoded progress event.
-- 规范化 msgpack 解码后的 progress event。
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

local function monotonic_event(event)
	local stage = event.stage
	if type(stage) ~= "string" or stage == "" then
		return event
	end

	local previous = stage_progress[stage]
	if previous then
		if event.total > 0 and previous.total > 0 and event.total < previous.total then
			event.total = previous.total
		end
		if event.current < previous.current then
			event.current = previous.current
		end
	end

	stage_progress[stage] = {
		current = event.current,
		total = event.total,
	}
	return event
end

-- Build a lookup table from Rust's phase plan.
-- 根据 Rust 传来的阶段计划构造查找表。
function M.handle_plan(plan)
	local items = normalize_plan(plan)
	if type(items) ~= "table" then
		return
	end

	reset()
	phases = {}
	phase_order = {}

	for _, raw_phase in ipairs(items) do
		local phase = normalize_phase(raw_phase)
		local name = phase and phase.name
		if name then
			table.insert(phase_order, name)
			phases[name] = {
				label = phase.label or name,
				weight = tonumber(phase.weight) or 0,
			}
		end
	end

	if vim.tbl_isempty(phases) then
		load_default_plan()
	end
end

-- Convert phase-local progress into an overall percentage.
-- 把阶段内进度换算成整体百分比。
local function overall_percent(event)
	local stage = event.stage

	if stage == "complete" then
		return 100
	end

	local phase = phases[stage]
	if not phase then
		local current = tonumber(event.current) or 0
		local total = tonumber(event.total) or 100
		return clamp(math.floor((current / math.max(total, 1)) * 100), 0, 100)
	end

	local before = 0
	for _, name in ipairs(phase_order) do
		if name == stage then
			break
		end

		before = before + ((phases[name] and phases[name].weight) or 0)
	end

	local current = tonumber(event.current) or 0
	local total = tonumber(event.total) or 100
	local local_ratio = clamp(current / math.max(total, 1), 0, 1)
	local percent = (before + local_ratio * phase.weight) * 100

	-- Keep progress monotonic even if Rust reuses a stage later.
	-- 即使 Rust 后面复用某个阶段，也保持整体百分比不回退。
	return clamp(math.floor(percent), last_percent, 100)
end

local function normalize_detail(message)
	message = tostring(message or "")
	return vim.trim(message)
end

local function event_numbers(event)
	local current = tonumber(event.current) or 0
	local total = tonumber(event.total) or 0
	return current, total
end

local function stage_label(stage)
	local phase = phases[stage]
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

	if #words == 0 then
		return nil
	end

	return table.concat(words, " ")
end

local function target_prefix()
	if target_kind == "engine" then
		return "UCore Engine"
	end

	return "UCore Project"
end

local function asset_stage_label(detail)
	local normalized = normalize_detail(detail):lower()
	if normalized:find("persist", 1, true) or normalized:find("ready", 1, true) then
		return "Asset Persist"
	end
	return "Asset Scan"
end

local function display_title_for_event(event, detail)
	local prefix = target_prefix()
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

	return prefix .. " " .. (stage_label(stage) or "Progress")
end

local function stage_percent(event)
	local current = tonumber(event.current) or 0
	local total = tonumber(event.total) or 100
	return clamp(math.floor((current / math.max(total, 1)) * 100), 0, 100)
end

local function format_progress_message(display_title, percent)
	return string.format("%s %d%%", display_title, percent)
end

-- Show user-facing progress notifications, throttled by overall percentage.
-- 按整体百分比节流显示面向用户的进度通知。
function M.handle_progress(event)
	event = monotonic_event(normalize_event(event))

	local progress_config = config.values.progress or {}
	if progress_config.enable == false then
		return
	end

	if not active then
		reset()
	end

	if event.stage == "complete" then
		active = false
		last_percent = 100
		last_stage = "complete"
		last_detail = nil
		last_tail = normalize_detail(event.message)
		if auto_finish then
			return M.finish()
		end
		return
	end

	local overall = overall_percent(event)
	local is_complete = overall >= 100
	local detail = normalize_detail(event.message)
	local display_title = display_title_for_event(event, detail)
	local rendered = format_progress_message(display_title, stage_percent(event))
	local same_render = overall == last_percent and event.stage == last_stage and rendered == (last_detail or "")

	-- Rust owns progress throttling; Lua ignores only stale or truly duplicate events.
	-- Rust 负责节流；Lua 只忽略过期或完全重复的事件。
	if not is_complete and (overall < last_percent or same_render) then
		return
	end

	last_percent = overall
	last_stage = event.stage
	last_detail = rendered
	last_tail = rendered:match("\n%-%-%-%- (.+)$") or detail

	log.write_progress("progress-ui", {
		stage = event.stage,
		current = event.current,
		total = event.total,
		overall = overall,
		display_title = display_title,
		detail = detail,
	})

	if current_display_title and current_display_title ~= display_title and visible then
		status.progress_finish(current_display_title, string.format("%s 100%%", current_display_title))
	end
	current_display_title = display_title

	if is_complete then
		return M.finish(string.format("%s 100%%", display_title))
	end

	if visible then
		status.progress(display_title, rendered)
	end
end

return M
