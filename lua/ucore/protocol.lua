local M = {}

M.version = 2

function M.is_compatible(status)
	if type(status) ~= "table" then
		return false
	end

	return tonumber(status.protocol_version) == M.version
end

function M.expected_label()
	return string.format("protocol v%d", M.version)
end

function M.status_label(status)
	if type(status) ~= "table" then
		return "unknown server status"
	end

	local protocol_version = tonumber(status.protocol_version)
	local server_version = tostring(status.server_version or "unknown")
	local build_id = tostring(status.build_id or "unknown")

	if protocol_version then
		return string.format("protocol v%d, server %s, build %s", protocol_version, server_version, build_id)
	end

	return string.format("legacy server response, server %s, build %s", server_version, build_id)
end

function M.compatibility_error(status)
	return string.format(
		"UCore server protocol mismatch: expected %s, got %s",
		M.expected_label(),
		M.status_label(status)
	)
end

return M
