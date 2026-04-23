local config = require("ucore.config")

local M = {}

-- Default refresh phase weights used when the Rust plan has not arrived yet.
-- 当 Rust 阶段计划还没到时使用的默认整体进度权重。
local default_phase_order = {
	"discovery",
	"db_sync",
	"analysis",
	"finalizing",
}

local default_phases = {
	discovery = { label = "Discovery", weight = 0.05 },
	db_sync = { label = "DB Sync", weight = 0.15 },
	analysis = { label = "Analysis", weight = 0.65 },
	finalizing = { label = "Finalizing", weight = 0.15 },
}

local phases = {}
local phase_order = {}
local last_percent = -1
local active = false
local title = "UCore refresh"

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
	active = true
end

-- Start a new visible progress run with a user-facing title.
-- 使用面向用户的标题开始一次新的进度展示。
function M.start(next_title)
	title = next_title or "UCore refresh"
	reset()
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
	local step = progress_config.notify_every_percent or 1
	local is_complete = overall >= 100 or event.stage == "complete"

	-- Keep internal Rust stages hidden from users; report only big-picture progress.
	-- 隐藏 Rust 内部阶段，只向用户展示整体刷新进度。
	if not is_complete and overall < last_percent + step then
		return
	end

	last_percent = overall

	local message = string.format("%s %d%%", title, overall)
	if is_complete then
		message = string.format("%s complete", title)
	end

	vim.notify(message, vim.log.levels.INFO)

	if is_complete then
		active = false
	end
end

return M
