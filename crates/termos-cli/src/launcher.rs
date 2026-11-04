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
use termos_core::app::{AccountInfo, App, LaunchContext, LaunchKind, Launcher};
use termos_core::journal::Journal;
use termos_devkit::accounts::Accounts;
use termos_devkit::cli_agents::Tool;

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
            LaunchKind::Usage => {
                let paths = ctx.paths.clone();
                let journal = Arc::clone(&self.journal);
                Ok(Box::new(termos_center::usage::UsageApp::open(Box::new(
                    move || {
                        Box::new(termos_center::usage_data::LiveUsage::new(paths, journal))
                            as Box<dyn termos_center::usage::UsageSource>
                    },
                ))))
            }
            LaunchKind::Packages => {
                let backend = termos_devkit::packages::System {
                    data_dir: ctx.paths.data_dir.clone(),
                    termos: std::env::current_exe()
                        .map_err(|e| format!("cannot find the termos program: {e}"))?,
                };
                Ok(Box::new(termos_devkit::packages_app::PackagesApp::open(
                    std::sync::Arc::new(backend),
                    None,
                )))
            }
            LaunchKind::Harness => {
                let (secrets, helper, config_file, home) = self.agents.shared(ctx);
                let project = ctx
                    .cwd
                    .map(std::path::Path::to_owned)
                    .or_else(|| home.clone())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let context = termos_harness::runner::Context {
                    api: Some(termos_harness::backends::ApiContext {
                        agents: ctx.config.agents.clone(),
                        secrets,
                        paths: ctx.paths.clone(),
                        helper,
                        config_file,
                        home,
                        journal: Some(Arc::clone(&self.journal)),
                    }),
                    config: ctx.config.clone(),
                    programs: Arc::new(crate::harness_cmd::ShimPrograms {
                        paths: ctx.paths.clone(),
                    }),
                    state_dir: ctx.paths.state_dir.clone(),
                    journal: Some(Arc::clone(&self.journal)),
                    project,
                };
                let h = termos_harness::runner::Harness::start(context)
                    .map_err(|e| format!("cannot start the harness: {e}"))?;
                Ok(Box::new(termos_harness::app::HarnessApp::open(h, arg)))
            }
            _ => self.agents.open(kind, arg, ctx),
        }
    }

    fn accounts(&self, ctx: &LaunchContext<'_>) -> Vec<AccountInfo> {
        let Ok(all) = Accounts::load(&ctx.paths.data_dir) else {
            return Vec::new();
        };
        Tool::ALL
            .into_iter()
            .flat_map(|tool| {
                let active = all.active(tool).name;
                all.list(tool).into_iter().map(move |a| AccountInfo {
                    cli: tool.name().to_owned(),
                    cli_title: tool.title().to_owned(),
                    title: a.title(),
                    active: a.name == active,
                    name: a.name,
                })
            })
            .collect()
    }

    fn use_account(
        &self,
        cli: &str,
        name: &str,
        ctx: &LaunchContext<'_>,
    ) -> Result<String, String> {
        let tool = Tool::from_name(cli).ok_or_else(|| format!("no accounts for {cli}"))?;
        let mut all = Accounts::load(&ctx.paths.data_dir).map_err(|e| e.to_string())?;
        let a = all.use_account(tool, name).map_err(|e| e.to_string())?;
        Ok(format!(
            "{}: {} is in use for new windows",
            tool.title(),
            a.title()
        ))
    }
}
