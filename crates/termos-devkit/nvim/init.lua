-- SPDX-License-Identifier: GPL-3.0-or-later
-- The Neovim TermOS manages (`termos edit`, DECISIONS.md D-012, D-054).
-- TermOS rewrites this file and lua/termos/ when it sets the editor up.
-- Put your own settings in lua/user.lua: TermOS creates it once and never
-- touches it again.

vim.g.mapleader = " "
vim.g.maplocalleader = " "

local o = vim.opt
o.number = true
o.relativenumber = false
o.signcolumn = "yes"
o.termguicolors = true
o.mouse = "a"
o.expandtab = true
o.shiftwidth = 4
o.tabstop = 4
o.smartindent = true
o.ignorecase = true
o.smartcase = true
o.updatetime = 300
o.splitright = true
o.splitbelow = true
o.undofile = true

require("termos.plugins")
require("termos.lsp")
require("termos.dap")

pcall(require, "user")
