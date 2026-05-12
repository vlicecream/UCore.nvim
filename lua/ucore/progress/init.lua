local config = require("ucore.config")
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
local active = false
local title = "UCore refresh"
local visible = true

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
	active = true
end

-- Start a new visible progress run with a user-facing title.
-- 使用面向用户的标题开始一次新的进度展示。
function M.start(next_title, opts)
	opts = opts or {}
	title = next_title or "UCore refresh"
	visible = opts.silent ~= true
	reset()
	if visible then
		local message = string.format("%s 0%%", title)
		local detail = vim.trim(tostring(opts.detail or ""))
		if detail ~= "" then
			message = message .. "\n---- " .. detail
		end
		status.progress(title, message)
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
	last_detail = nil
	if visible then
		status.progress_finish(title, message or string.format("%s 100%%", title))
	end
end

-- Mark the current progress run as failed.
-- 标记当前进度展示失败。
function M.fail(message)
	active = false
	last_stage = "failed"
	last_detail = nil
	if visible then
		status.progress_fail(title, message or string.format("%s failed", title))
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
		current = event.current or event[3],
		total = event.total or event[4],
		message = event.message or event[5],
	}
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

local function format_progress_message(overall, event)
	local lines = { string.format("%s %d%%", title, overall) }
	local label = stage_label(event.stage)
	local current, total = event_numbers(event)
	local detail = normalize_detail(event.message)
	local computed_detail = nil

	if total > 0 and event.stage ~= "complete" then
		local stage_percent = clamp(math.floor((current / math.max(total, 1)) * 100), 0, 100)
		local prefix = label or tostring(event.stage or "Progress")
		computed_detail = string.format("%s %d/%d (%d%%)", prefix, current, total, stage_percent)
	end

	if computed_detail and detail ~= "" then
		local lower_detail = detail:lower()
		local lower_prefix = computed_detail:lower()
		if lower_detail ~= lower_prefix and not lower_detail:find(lower_prefix, 1, true) then
			detail = string.format("%s | %s", computed_detail, detail)
		else
			detail = computed_detail
		end
	elseif computed_detail then
		detail = computed_detail
	elseif detail ~= "" and label then
		local lower_detail = detail:lower()
		local lower_label = label:lower()
		local lower_stage = tostring(event.stage or ""):lower()
		if not lower_detail:find(lower_label, 1, true) and (lower_stage == "" or not lower_detail:find(lower_stage, 1, true)) then
			detail = string.format("%s: %s", label, detail)
		end
	end

	if detail == "" then
		return table.concat(lines, "\n")
	end

	table.insert(lines, "---- " .. detail)
	return table.concat(lines, "\n")
end

-- Show user-facing progress notifications, throttled by overall percentage.
-- 按整体百分比节流显示面向用户的进度通知。
function M.handle_progress(event)
	event = normalize_event(event)

	local progress_config = config.values.progress or {}
	if progress_config.enable == false then
		return
	end

	if not active then
		reset()
	end

	local overall = overall_percent(event)
	local is_complete = overall >= 100 or event.stage == "complete"
	local detail = normalize_detail(event.message)
	local rendered = format_progress_message(overall, event)
	local same_render = overall == last_percent and event.stage == last_stage and rendered == (last_detail or "")

	-- Rust owns progress throttling; Lua ignores only stale or truly duplicate events.
	-- Rust 负责节流；Lua 只忽略过期或完全重复的事件。
	if not is_complete and (overall < last_percent or same_render) then
		return
	end

	last_percent = overall
	last_stage = event.stage
	last_detail = rendered

	if is_complete then
		return M.finish(string.format("%s 100%%\n---- %s", title, detail ~= "" and detail or "Complete."))
	end

	if visible then
		status.progress(title, rendered)
	end
end

return M
