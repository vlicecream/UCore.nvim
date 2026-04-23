local cli = require("ucore.client.cli")
local rpc = require("ucore.client.rpc")

local M = {
	cli = cli,
	rpc = rpc,
}

-- Keep most lifecycle commands on the CLI bridge, but run refresh over RPC so
-- progress notifications can reach Neovim.
-- 大多数生命周期命令仍走 CLI 桥；refresh 走 RPC，方便把进度通知送回 Neovim。
M.request = cli.request
M.status = cli.status
M.setup = cli.setup
function M.refresh(payload, callback)
	rpc.request("refresh", payload, function(result, err)
		if not err then
			return callback(result, nil)
		end

		local text = tostring(err)
		if text:find("ECONNREFUSED", 1, true) or text:lower():find("connection refused", 1, true) then
			return cli.refresh(payload, callback)
		end

		callback(nil, err)
	end)
end
M.watch = cli.watch
M.query = cli.query

return M
