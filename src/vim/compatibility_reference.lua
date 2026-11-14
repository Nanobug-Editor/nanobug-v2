-- Run documented inputs in a clean Neovim process for differential tests only.
local api, fn = vim.api, vim.fn
local cases = vim.json.decode(table.concat(fn.readfile(arg[1]), "\n"))
local states = {}

vim.o.lines = 9
vim.o.columns = 80
vim.o.laststatus = 0
vim.o.showtabline = 0
vim.o.cmdheight = 1
vim.o.wrap = false
vim.o.scrolloff = 3
vim.o.tabstop = 4
vim.o.shiftwidth = 4
vim.o.more = false
if fn.has("win32") == 1 then
  vim.o.shell = "cmd.exe"
  vim.o.shellcmdflag = "/d /s /c"
  vim.o.shellquote = ""
  vim.o.shellxquote = '"'
  vim.o.shellredir = ">%s 2>&1"
else
  vim.o.shell = "sh"
  vim.o.shellcmdflag = "-c"
end

local function feed(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, true, true), "mxt", false)
end

local function register(name)
  local shape = fn.getregtype(name)
  return { fn.getreg(name), shape == "" and "v" or shape }
end

for index, case in ipairs(cases) do
  if fn.mode() ~= "n" then feed("<Esc>") end
  local path = ("case-%d.txt"):format(index)
  fn.writefile(vim.split(case.text, "\n", { plain = true }), path, "b")
  local window
  if case.close then
    vim.cmd("silent split " .. path)
    window = api.nvim_get_current_win()
  else
    vim.cmd("silent edit! " .. path)
  end
  vim.bo.endofline = case.text:sub(-1) == "\n"
  vim.bo.fixendofline = false
  vim.bo.modified = false
  vim.cmd("clearjumps")
  fn.setreg("a", "")
  fn.setreg('"', "")
  fn.setreg("/", "")
  vim.o.lines = 9
  if case.top ~= vim.NIL then
    assert(api.nvim_win_get_height(0) == 8, "unexpected reference viewport height")
  end
  vim.wo.scroll = math.max(1, math.floor(api.nvim_win_get_height(0) / 2))
  api.nvim_win_set_cursor(0, { case.row + 1, case.col })
  if case.top ~= vim.NIL then
    fn.winrestview({ lnum = case.row + 1, col = case.col, topline = case.top + 1 })
  end
  vim.cmd.redraw()
  local initial = fn.winsaveview().topline - 1
  feed(case.keys)
  vim.cmd.redraw()
  local row, col = unpack(api.nvim_win_get_cursor(0))
  local mode = fn.mode()
  local anchor = vim.NIL
  if mode == "v" or mode == "V" or mode == "\22" then
    local position = fn.getpos("v")
    anchor = { position[2] - 1, position[3] - 1 }
  end
  local text = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), "\n")
  if vim.bo.endofline then text = text .. "\n" end
  if case.close then
    assert(not api.nvim_win_is_valid(window), "reference did not close scratch window")
    text = table.concat(fn.readfile(path, "b"), "\n")
    mode, row, col = "closed", 1, 0
  end
  states[#states + 1] = {
    text = text,
    caret = { row - 1, col },
    mode = mode,
    anchor = anchor,
    unnamed = register('"'),
    named = register("a"),
    top = case.top ~= vim.NIL and fn.winsaveview().topline - 1 or vim.NIL,
    initial_top = initial,
  }
end

local version = vim.version()
fn.writefile({ vim.json.encode({ version = ("%d.%d.%d"):format(version.major, version.minor, version.patch), states = states }) }, arg[2])
