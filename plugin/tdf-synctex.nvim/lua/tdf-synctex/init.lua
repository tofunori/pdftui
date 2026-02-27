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

	vim.api.nvim_create_user_command("TdfOpen", function(args)
		local pdf = args.args ~= "" and args.args or nil
		M.open_in_tmux(pdf)
	end, { nargs = "?", complete = "file", desc = "Open PDF in tdf-sync (tmux pane)" })
end

--- Auto-detect the PDF path from the current .tex file
---@return string|nil
local function detect_pdf()
	local tex = vim.fn.expand("%:p")
	if tex == "" then return nil end
	local pdf = tex:gsub("%.tex$", ".pdf")
	if vim.fn.filereadable(pdf) == 1 then
		return pdf
	end
	-- Try looking in the same directory for any .pdf matching the basename
	local dir = vim.fn.fnamemodify(tex, ":h")
	local stem = vim.fn.fnamemodify(tex, ":t:r")
	local candidate = dir .. "/" .. stem .. ".pdf"
	if vim.fn.filereadable(candidate) == 1 then
		return candidate
	end
	return nil
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
	local pdf = M.config.pdf_path or detect_pdf()
	if not pdf then
		vim.notify("tdf-synctex: no PDF found (compile first or set pdf_path)", vim.log.levels.ERROR)
		return
	end
	M.config.pdf_path = pdf

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

--- Open the PDF in tdf-sync in a new tmux pane (right split)
--- Ctrl+click in the viewer will jump back to Neovim via nvr
---@param pdf string|nil path to the PDF file (auto-detected if nil)
function M.open_in_tmux(pdf)
	pdf = pdf or M.config.pdf_path or detect_pdf()
	if not pdf then
		vim.notify("tdf-synctex: no PDF found", vim.log.levels.ERROR)
		return
	end
	M.config.pdf_path = pdf

	local servername = vim.v.servername
	local inverse_cmd = string.format(
		"nvr --servername %s --remote-silent +{line} {file}",
		servername
	)

	-- Launch in a tmux split-pane to the right (40% width)
	local tmux_cmd = string.format(
		"tmux split-window -h -l 40%% '%s --inverse-cmd %s %s'",
		M.config.viewer_cmd,
		vim.fn.shellescape(inverse_cmd),
		vim.fn.shellescape(pdf)
	)
	vim.fn.system(tmux_cmd)
end

return M
