// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the `local` provider points when its config names no address:
//! TermOS's model router (`termos models serve`, D-068).
//!
//! The router writes its address and a per-start token to
//! `<runtime>/models.json` (owner-only). When no live router is found, the
//! `termos` binary is asked to start one in the background, and the file
//! is awaited. The runtime folder is hidden from agents' commands, so the
//! token stays with TermOS.

use std::path::Path;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use termos_core::models_endpoint::{self, ModelsEndpoint};
use termos_core::paths::Paths;

use super::{ErrorKind, ProviderError};

/// How long a router may take to start listening (llama.cpp is installed
/// later, on the first request for a GGUF model, so this is short).
const START: Duration = Duration::from_secs(30);

/// The router's endpoint, if its file names one that answers.
pub async fn live(paths: &Paths) -> Option<ModelsEndpoint> {
    let e = models_endpoint::read(paths)?;
    let health = format!("{}/health", e.url.trim_end_matches("/v1"));
    let client = super::http::client().ok()?;
    let r = client
        .get(health)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    r.status().is_success().then_some(e)
}

/// The router's base URL and token, starting it with `helper` (the
/// `termos` binary, given `config` as its config file) if none runs.
pub async fn router(
    paths: &Paths,
    helper: Option<&Path>,
    config: Option<&Path>,
) -> Result<(String, SecretString), ProviderError> {
    if let Some(e) = live(paths).await {
        return Ok((e.url, e.token));
    }
    let Some(helper) = helper else {
        return Err(ProviderError::new(
            ErrorKind::Config,
            "no local model router runs: start one with `termos models serve`, \
             or give the provider a `base-url`",
        ));
    };
    let mut cmd = std::process::Command::new(helper);
    if let Some(c) = config {
        cmd.arg("--config").arg(c);
    }
    cmd.args(["models", "serve", "--daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|e| {
        ProviderError::new(
            ErrorKind::Config,
            format!("cannot start the model router ({}): {e}", helper.display()),
        )
    })?;
    tracing::info!("started the model router (pid {})", child.id());
    let deadline = Instant::now() + START;
    let outcome = loop {
        if let Some(e) = live(paths).await {
            break Ok((e.url, e.token));
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It found another router that started meanwhile, or failed.
            break live(paths).await.map(|e| (e.url, e.token)).ok_or_else(|| {
                ProviderError::new(
                    ErrorKind::Config,
                    format!(
                        "the model router stopped ({status}) while starting: its log is \
                         models.log.<date> in {}",
                        paths.log_dir().display()
                    ),
                )
            });
        }
        if Instant::now() > deadline {
            break Err(ProviderError::new(
                ErrorKind::Config,
                format!("the model router did not start within {}s", START.as_secs()),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // It outlives this session; reap it when it stops.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths {
            config_dir: dir.join("c"),
            state_dir: dir.join("s"),
            runtime_dir: dir.join("run"),
            data_dir: dir.join("d"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stale_file_is_not_a_router() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(dir.path());
        // A port nobody listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("port")
            .port();
        models_endpoint::write(
            &p,
            &ModelsEndpoint {
                url: format!("http://127.0.0.1:{port}/v1"),
                token: SecretString::from("t".repeat(32)),
                pid: 1,
            },
        )
        .expect("write");
        assert!(live(&p).await.is_none());
        let e = router(&p, None, None).await.expect_err("no helper");
        assert!(e.message.contains("termos models serve"), "{}", e.message);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn the_helper_is_started_and_its_file_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let p = paths(dir.path());
        // A stand-in `termos`: answers /health and writes the endpoint file
        // with the arguments it got.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for s in listener.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                );
            }
        });
        let file = models_endpoint::path(&p);
        let helper = dir.path().join("termos");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nmkdir -p '{dir}'\nprintf '{{\"url\":\"http://127.0.0.1:{port}/v1\",\"token\":\"%s\",\"pid\":1}}' \"$*\" > '{file}'\n",
                dir = file.parent().expect("dir").display(),
                file = file.display(),
            ),
        )
        .expect("write");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        use secrecy::ExposeSecret as _;

        let (url, token) = router(&p, Some(&helper), Some(Path::new("/x/config.toml")))
            .await
            .expect("router");
        assert_eq!(url, format!("http://127.0.0.1:{port}/v1"));
        assert_eq!(
            token.expose_secret(),
            "--config /x/config.toml models serve --daemon"
        );
    }
}
