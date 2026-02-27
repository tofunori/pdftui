local M = {}

M.config = {
	socket_path = nil, -- auto-discovered via tdf --socket-path <pdf>
	pdf_path = nil,
	viewer_cmd = "tdf-sync",
}

function M.setup(opts)
	M.config = vim.tbl_deep_extend("force", M.config, opts or {})

	vim.api.nvim_create_user_command("TdfForward", function()
		M.forward_search()
	end, { desc = "SyncTeX forward search to tdf viewer" })
end

--- Discover the IPC socket path for a given PDF
---@param pdf string
---@return string|nil
local function discover_socket(pdf)
	local result = vim.fn.system({ "tdf", "--socket-path", pdf })
	if vim.v.shell_error ~= 0 then
		return nil
	end
	return vim.trim(result)
end

--- Send a forward search command to the running viewer
function M.forward_search()
	local pdf = M.config.pdf_path
	if not pdf then
		vim.notify("tdf-synctex: pdf_path not set", vim.log.levels.ERROR)
		return
	end

	local sock = M.config.socket_path or discover_socket(pdf)
	if not sock then
		vim.notify("tdf-synctex: cannot determine socket path", vim.log.levels.ERROR)
		return
	end

	local line = vim.fn.line(".")
	local col = vim.fn.col(".") - 1
	local file = vim.fn.expand("%:p")

	local cmd = string.format(
		'echo "forward %d %d %s" | socat - UNIX-CONNECT:%s',
		line, col, file, sock
	)
	vim.fn.jobstart(cmd, {
		on_stderr = function(_, data)
			local msg = table.concat(data, "\n")
			if msg ~= "" then
				vim.notify("tdf-synctex: " .. msg, vim.log.levels.WARN)
			end
		end,
	})
end

--- Open a PDF in tdf-sync in a terminal split with inverse search configured
---@param pdf string path to the PDF file
function M.open_pdf(pdf)
	M.config.pdf_path = pdf

	local servername = vim.v.servername
	local inverse_cmd = string.format(
		"nvr --servername %s --remote-silent +{line} {file}",
		servername
	)

	local cmd = string.format(
		"%s --inverse-cmd %s %s",
		M.config.viewer_cmd,
		vim.fn.shellescape(inverse_cmd),
		vim.fn.shellescape(pdf)
	)

	vim.cmd("vsplit | terminal " .. cmd)
end

return M
