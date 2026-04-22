local cli = require("ucore.client.cli")
local rpc = require("ucore.client.rpc")

local M = {
	cli = cli,
	rpc = rpc,
}

-- Keep the public client API on the CLI bridge for lifecycle commands.
-- 生命周期命令先保持走 CLI 桥，稳定且方便调试。
M.request = cli.request
M.status = cli.status
M.setup = cli.setup
M.refresh = cli.refresh
M.watch = cli.watch
M.query = cli.query

return M
