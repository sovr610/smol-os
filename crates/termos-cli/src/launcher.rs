// SPDX-License-Identifier: GPL-3.0-or-later
//! The windows the shell opens from crates it does not depend on
//! (ARCHITECTURE.md §4): agent windows and audit logs (termos-ai) and the
//! command center (termos-center, D-093), which share the journal and the
//! live agent registry made here.

use std::sync::Arc;

use termos_ai::registry::Registry;
use termos_ai::ui::AgentLauncher;
use termos_center::app::Agents;
use termos_center::data::{Gather, Live};
use termos_core::app::{App, LaunchContext, LaunchKind, Launcher};
use termos_core::journal::Journal;

/// Every launched window of the `termos` binary.
pub(crate) struct Windows {
    pub(crate) agents: AgentLauncher,
    pub(crate) journal: Arc<Journal>,
    pub(crate) registry: Arc<Registry>,
}

impl Launcher for Windows {
    fn open(
        &self,
        kind: LaunchKind,
        arg: &str,
        ctx: &LaunchContext<'_>,
    ) -> Result<Box<dyn App>, String> {
        match kind {
            LaunchKind::Center => {
                let socket = ctx.socket.map(std::path::Path::to_owned);
                let journal = Arc::clone(&self.journal);
                let paths = ctx.paths.clone();
                let config = ctx.config.clone();
                let agents: Arc<dyn Agents> = Arc::clone(&self.registry) as Arc<dyn Agents>;
                Ok(Box::new(termos_center::CommandCenter::open(
                    Box::new(move || {
                        Box::new(Live::new(socket, journal, paths, config)) as Box<dyn Gather>
                    }),
                    agents,
                )))
            }
            _ => self.agents.open(kind, arg, ctx),
        }
    }
}
