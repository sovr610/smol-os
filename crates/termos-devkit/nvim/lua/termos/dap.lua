-- SPDX-License-Identifier: GPL-3.0-or-later
-- Debugging (nvim-dap): the debuggers `termos lang install` installed.
-- F5 start/continue · F9 breakpoint · F10 over · F11 into · F12 out

local ok, dap = pcall(require, "dap")
if not ok then
  return
end
local generated = require("termos.generated")

local function rest(list)
  local out = {}
  for i = 2, #list do
    out[#out + 1] = list[i]
  end
  return out
end

-- How each debugger starts a program.
local launches = {
  delve = {
    { type = "delve", name = "Debug this file", request = "launch", program = "${file}" },
    { type = "delve", name = "Debug this package", request = "launch", program = "${fileDirname}" },
    { type = "delve", name = "Debug the tests here", request = "launch", mode = "test", program = "${fileDirname}" },
  },
  codelldb = {
    {
      type = "codelldb",
      name = "Debug a program",
      request = "launch",
      cwd = "${workspaceFolder}",
      program = function()
        return vim.fn.input("Program: ", vim.fn.getcwd() .. "/", "file")
      end,
    },
  },
  debugpy = {
    { type = "debugpy", name = "Debug this file", request = "launch", program = "${file}", console = "integratedTerminal" },
  },
  ["pwa-node"] = {
    { type = "pwa-node", name = "Debug this file", request = "launch", program = "${file}", cwd = "${workspaceFolder}" },
  },
  netcoredbg = {
    {
      type = "netcoredbg",
      name = "Debug a program",
      request = "launch",
      program = function()
        return vim.fn.input("Program (.dll): ", vim.fn.getcwd() .. "/bin/Debug/", "file")
      end,
    },
  },
}

for _, d in ipairs(generated.debuggers) do
  if d.port_args then
    dap.adapters[d.name] = {
      type = "server",
      port = "${port}",
      executable = { command = d.cmd[1], args = vim.list_extend(rest(d.cmd), d.port_args) },
    }
  else
    dap.adapters[d.name] = { type = "executable", command = d.cmd[1], args = rest(d.cmd) }
  end
  for _, ft in ipairs(d.filetypes) do
    dap.configurations[ft] = launches[d.name] or {}
  end
end

local okui, dapui = pcall(require, "dapui")
if okui then
  dapui.setup()
  dap.listeners.after.event_initialized["termos-dapui"] = function() dapui.open() end
  dap.listeners.before.event_terminated["termos-dapui"] = function() dapui.close() end
  dap.listeners.before.event_exited["termos-dapui"] = function() dapui.close() end
end

vim.keymap.set("n", "<F5>", dap.continue, { desc = "Debug: start or continue" })
vim.keymap.set("n", "<F9>", dap.toggle_breakpoint, { desc = "Debug: breakpoint" })
vim.keymap.set("n", "<leader>b", dap.toggle_breakpoint, { desc = "Debug: breakpoint" })
vim.keymap.set("n", "<F10>", dap.step_over, { desc = "Debug: step over" })
vim.keymap.set("n", "<F11>", dap.step_into, { desc = "Debug: step into" })
vim.keymap.set("n", "<F12>", dap.step_out, { desc = "Debug: step out" })
