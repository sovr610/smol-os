// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging to a file. The desktop owns the terminal, so nothing may be
//! written to stdout or stderr while it runs (DECISIONS.md D-017).

use std::path::Path;

use tracing_subscriber::EnvFilter;

/// Sends `tracing` output to `<dir>/termos.log.<date>` (daily rotation, three
/// files kept). The level comes from `TERMOS_LOG` (default `info`), using
/// `tracing` filter syntax, e.g. `TERMOS_LOG=termos_shell=debug`.
pub fn init_file_logging(dir: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("termos.log")
        .max_log_files(3)
        .build(dir)?;
    let filter = EnvFilter::try_from_env("TERMOS_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(appender)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()?;
    Ok(())
}
