-- SPDX-License-Identifier: GPL-3.0-or-later
-- Language servers: the ones `termos lang install` installed, started
-- through mise with the exact versions installed (lua/termos/generated.lua).

local generated = require("termos.generated")

-- Markers such as "*.csproj" are patterns, which root_markers takes
-- literally: a root_dir function looks for them instead.
local function patterned(markers)
  for _, m in ipairs(markers) do
    if m:find("[*?]") then return true end
  end
  return false
end

local function root_dir(markers, required)
  local pats = {}
  for _, m in ipairs(markers) do
    pats[#pats + 1] = vim.glob.to_lpeg(m)
  end
  return function(bufnr, on_dir)
    local file = vim.api.nvim_buf_get_name(bufnr)
    local found = vim.fs.find(function(name)
      for _, p in ipairs(pats) do
        if p:match(name) then return true end
      end
      return false
    end, { upward = true, path = vim.fs.dirname(file) })[1]
    if found then
      on_dir(vim.fs.dirname(found))
    elseif not required then
      on_dir(vim.fs.dirname(file))
    end
  end
end

for _, s in ipairs(generated.servers) do
  local config = {
    cmd = s.cmd,
    filetypes = s.filetypes,
    workspace_required = s.workspace_required,
  }
  if patterned(s.root_markers) then
    config.root_dir = root_dir(s.root_markers, s.workspace_required)
  else
    config.root_markers = s.root_markers
  end
  vim.lsp.config(s.name, config)
  vim.lsp.enable(s.name)
end

vim.diagnostic.config({
  virtual_text = true,
  severity_sort = true,
  float = { border = "rounded" },
})

-- Neovim's own keys work too: K (hover), grn (rename), gra (actions),
-- grr (references), gri (implementations), gO (symbols), [d ]d (diagnostics).
vim.api.nvim_create_autocmd("LspAttach", {
  group = vim.api.nvim_create_augroup("termos-lsp", { clear = true }),
  callback = function(ev)
    local opts = { buffer = ev.buf }
    vim.keymap.set("n", "gd", vim.lsp.buf.definition, opts)
    vim.keymap.set("n", "gD", vim.lsp.buf.declaration, opts)
    vim.keymap.set("n", "<leader>f", function() vim.lsp.buf.format({ async = true }) end, opts)
    vim.keymap.set("n", "<leader>e", vim.diagnostic.open_float, opts)
  end,
})
