// SPDX-License-Identifier: GPL-3.0-or-later
//! Child processes on pseudo-terminals (DECISIONS.md D-025).
//!
//! Each pane gets two threads that sleep in the kernel while idle:
//! - a reader that forwards output chunks to the server over a bounded
//!   channel (backpressure: a flooding child is slowed, not buffered without
//!   limit) and reports the exit status when the PTY closes;
//! - a writer that delivers input, so a child that stops reading can never
//!   block the desktop.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use termos_core::ids::PaneId;
use tokio::sync::mpsc;

/// What a pane's PTY reports to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output(PaneId, Vec<u8>),
    /// The child exited (exit code if known) and the PTY closed.
    Exited(PaneId, Option<u32>),
}

/// How to start a pane's program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (added to the inherited environment).
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl SpawnSpec {
    /// A login-less interactive shell (`program` with no arguments).
    pub fn shell(program: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            cols,
            rows,
        }
    }
}

/// Output chunks are at most this large.
const READ_CHUNK: usize = 64 * 1024;

/// A running child on a PTY.
pub struct PtyPane {
    id: PaneId,
    master: Box<dyn MasterPty + Send>,
    input: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
}

impl std::fmt::Debug for PtyPane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyPane")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl PtyPane {
    /// Starts `spec` on a new PTY. Output and exit arrive on `events`.
    pub fn spawn(id: PaneId, spec: &SpawnSpec, events: mpsc::Sender<PtyEvent>) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: spec.rows.max(1),
                cols: spec.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        } else if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(io_err)?;
        // Close our copy of the slave so EOF arrives when the child exits.
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(io_err)?;
        let mut writer = pair.master.take_writer().map_err(io_err)?;

        thread::Builder::new()
            .name(format!("pty-read-{}", id.0))
            .spawn(move || {
                let mut buf = vec![0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if events
                                .blocking_send(PtyEvent::Output(id, buf[..n].to_vec()))
                                .is_err()
                            {
                                // The server is gone.
                                return;
                            }
                        }
                    }
                }
                let code = child.wait().ok().map(|s| s.exit_code());
                let _ = events.blocking_send(PtyEvent::Exited(id, code));
            })?;

        let (input, input_rx) = std_mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name(format!("pty-write-{}", id.0))
            .spawn(move || {
                while let Ok(bytes) = input_rx.recv() {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;

        Ok(Self {
            id,
            master: pair.master,
            input,
            killer,
            pid,
        })
    }

    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Queues bytes for the child's input. Never blocks.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            // If the writer thread ended, the child is exiting; drop input.
            let _ = self.input.send(bytes);
        }
    }

    /// Resizes the PTY (the child receives SIGWINCH).
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io_err)
    }

    /// Terminates the child.
    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    /// The PID of the PTY's foreground process group leader (the program
    /// the user is interacting with), falling back to the child. Windows
    /// has no foreground process group: always the child.
    pub fn foreground_pid(&self) -> Option<u32> {
        #[cfg(unix)]
        let leader = self
            .master
            .process_group_leader()
            .and_then(|p| u32::try_from(p).ok());
        #[cfg(not(unix))]
        let leader = None;
        leader.or(self.pid)
    }

    /// Name of the foreground program (Linux; `None` elsewhere).
    pub fn foreground_name(&self) -> Option<String> {
        let pid = self.foreground_pid()?;
        proc_comm(pid)
    }

    /// Working directory of the foreground program (Linux; `None` elsewhere).
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        let pid = self.foreground_pid()?;
        proc_cwd(pid)
    }

    /// The command line of the foreground program when it is not the
    /// pane's own program, e.g. `vim notes.md` started from the shell
    /// (Linux; `None` elsewhere and at the shell prompt).
    pub fn foreground_command(&self) -> Option<Vec<String>> {
        let pid = self.foreground_pid()?;
        if Some(pid) == self.pid {
            return None;
        }
        // Between fork and exec it is not the user's program yet.
        proc_comm(pid)?;
        proc_cmdline(pid)
    }
}

impl Drop for PtyPane {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(target_os = "linux")]
fn proc_comm(pid: u32) -> Option<String> {
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let name = read(&format!("/proc/{pid}/comm"))?;
    // Between fork and exec the child still carries the name of the thread
    // that spawned it (ours); that is not the program the user started.
    if read("/proc/thread-self/comm").as_deref() == Some(name.as_str()) {
        return None;
    }
    Some(name)
}

#[cfg(not(target_os = "linux"))]
fn proc_comm(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    (!argv.is_empty()).then_some(argv)
}

#[cfg(not(target_os = "linux"))]
fn proc_cmdline(_pid: u32) -> Option<Vec<String>> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(target_os = "linux"))]
fn proc_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn collect(rx: &mut mpsc::Receiver<PtyEvent>) -> (Vec<u8>, Option<u32>) {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                Ok(Some(PtyEvent::Output(_, bytes))) => out.extend(bytes),
                Ok(Some(PtyEvent::Exited(_, code))) => return (out, code),
                Ok(None) | Err(_) => {
                    panic!("pty did not exit; got {:?}", String::from_utf8_lossy(&out))
                }
            }
        }
    }

    #[tokio::test]
    async fn runs_a_program_and_reports_exit() {
        let (tx, mut rx) = mpsc::channel(16);
        let spec = SpawnSpec {
            args: vec![
                "-c".into(),
                "printf 'hi %s' \"$TERMOS_TEST\"; exit 3".into(),
            ],
            env: vec![("TERMOS_TEST".into(), "there".into())],
            ..SpawnSpec::shell("/bin/sh", 40, 5)
        };
        let _pane = PtyPane::spawn(PaneId(1), &spec, tx).expect("spawn");
        let (out, code) = collect(&mut rx).await;
        assert!(String::from_utf8_lossy(&out).contains("hi there"));
        assert_eq!(code, Some(3));
    }

    #[tokio::test]
    async fn input_reaches_the_child_and_size_is_applied() {
        let (tx, mut rx) = mpsc::channel(16);
        let spec = SpawnSpec {
            args: vec![
                "-c".into(),
                "read line; stty size; echo \"got:$line\"".into(),
            ],
            ..SpawnSpec::shell("/bin/sh", 50, 7)
        };
        let pane = PtyPane::spawn(PaneId(2), &spec, tx).expect("spawn");
        pane.write(b"hello\r".to_vec());
        let (out, code) = collect(&mut rx).await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("7 50"), "{text:?}");
        assert!(text.contains("got:hello"), "{text:?}");
        assert_eq!(code, Some(0));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reports_the_foreground_command() {
        let (tx, mut rx) = mpsc::channel(64);
        let spec = SpawnSpec::shell("/bin/sh", 40, 5);
        let pane = PtyPane::spawn(PaneId(3), &spec, tx).expect("spawn");
        // Keep the channel drained so the reader never blocks.
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        assert_eq!(pane.foreground_command(), None, "the shell itself");
        pane.write(b"sleep 30\r".to_vec());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(argv) = pane.foreground_command() {
                assert_eq!(argv, ["sleep", "30"]);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sleep never became the foreground program"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
