-- SPDX-License-Identifier: GPL-3.0-or-later
-- Language servers: the ones `termos lang install` installed, started
-- through mise with the exact versions installed (lua/termos/generated.lua).

local generated = require("termos.generated")

for _, s in ipairs(generated.servers) do
  vim.lsp.config(s.name, {
    cmd = s.cmd,
    filetypes = s.filetypes,
    root_markers = s.root_markers,
    workspace_required = s.workspace_required,
  })
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
