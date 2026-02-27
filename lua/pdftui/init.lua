local M = {}

M.config = {
	socket_path = nil,
	pdf_path = nil,
	viewer_cmd = "pdftui-sync",
	split = false, -- true = cmux split, false = cmux tab
}

function M.setup(opts)
	M.config = vim.tbl_deep_extend("force", M.config, opts or {})

	vim.api.nvim_create_user_command("PdftuiForward", function()
		M.forward_search()
	end, { desc = "SyncTeX forward search to pdftui viewer" })

	vim.api.nvim_create_user_command("PdftuiOpen", function(args)
		local pdf = args.args ~= "" and args.args or nil
		M.open(pdf)
	end, { nargs = "?", complete = "file", desc = "Open PDF in pdftui (cmux tab)" })

	vim.api.nvim_create_user_command("PdftuiSplit", function(args)
		local pdf = args.args ~= "" and args.args or nil
		M.open(pdf, true)
	end, { nargs = "?", complete = "file", desc = "Open PDF in pdftui (cmux split)" })
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
	return nil
end

--- Discover the IPC socket path for a given PDF
---@param pdf string
---@return string|nil
local function discover_socket(pdf)
	local result = vim.fn.system({ "pdftui", "--socket-path", pdf })
	if vim.v.shell_error ~= 0 then
		return nil
	end
	return vim.trim(result)
end

--- Send a forward search command to the running viewer
function M.forward_search()
	local pdf = M.config.pdf_path or detect_pdf()
	if not pdf then
		vim.notify("pdftui: no PDF found (compile first)", vim.log.levels.ERROR)
		return
	end
	M.config.pdf_path = pdf

	local line = vim.fn.line(".")
	local col = vim.fn.col(".") - 1
	local file = vim.fn.expand("%:p")

	vim.fn.jobstart({ "pdftui", "--forward", line .. ":" .. col .. ":" .. file, pdf }, {
		on_stderr = function(_, data)
			local msg = table.concat(data, "\n")
			if msg ~= "" then
				vim.notify("pdftui: " .. msg, vim.log.levels.WARN)
			end
		end,
	})
end

--- Open the PDF in pdftui via cmux (new tab or split)
---@param pdf string|nil path to the PDF (auto-detected if nil)
---@param split boolean|nil true for right split, false/nil for new tab
function M.open(pdf, split)
	pdf = pdf or M.config.pdf_path or detect_pdf()
	if not pdf then
		vim.notify("pdftui: no PDF found", vim.log.levels.ERROR)
		return
	end
	M.config.pdf_path = pdf

	local args = { "pdftui-open", pdf, "--nvim-server", vim.v.servername }
	if split or M.config.split then
		table.insert(args, "--split")
	end
	vim.fn.system(args)
end

return M
