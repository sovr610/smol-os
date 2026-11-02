-- SPDX-License-Identifier: GPL-3.0-or-later
-- Plugins, pinned to exact commits and installed by Neovim's own plugin
-- manager (vim.pack, Neovim 0.12). TermOS updates the pins.

if not vim.pack then
  vim.notify("TermOS: this Neovim has no vim.pack (needs 0.12); debugging is off", vim.log.levels.WARN)
  return
end

vim.pack.add({
  { src = "https://github.com/mfussenegger/nvim-dap", version = "6a5bba0ddea5d419a783e170c20988046376090d" },
  { src = "https://github.com/nvim-neotest/nvim-nio", version = "21f5324bfac14e22ba26553caf69ec76ae8a7662" },
  { src = "https://github.com/rcarriga/nvim-dap-ui", version = "f7d75cca202b52a60c520ec7b1ec3414d6e77b0f" },
}, { confirm = false })
