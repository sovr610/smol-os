// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs the real `termos` binary on a pseudo-terminal and parses its output
//! with an independent emulator (`vt100`), so tests see exactly what a user
//! would.

#![allow(dead_code, unreachable_pub)]

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

struct Shared {
    parser: vt100::Parser,
    raw: Vec<u8>,
    generation: u64,
    closed: bool,
}

/// An isolated home, config, state and runtime directory. Dropping it
/// stops the server started in it.
pub struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    pub fn new(extra_config: &str) -> Arc<Self> {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config").join("termos");
        std::fs::create_dir_all(&cfg).expect("mkdir");
        std::fs::write(
            cfg.join("config.toml"),
            format!(
                "[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n{extra_config}\n"
            ),
        )
        .expect("config");
        Arc::new(Self { dir })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The default server's socket.
    pub fn socket(&self) -> PathBuf {
        self.path().join("run").join("termos").join("default.sock")
    }

    fn vars(&self) -> Vec<(&'static str, PathBuf)> {
        let d = self.path();
        vec![
            ("HOME", d.to_owned()),
            ("XDG_CONFIG_HOME", d.join("config")),
            ("XDG_STATE_HOME", d.join("state")),
            ("XDG_RUNTIME_DIR", d.join("run")),
            ("SHELL", "/bin/sh".into()),
            ("TERM", "xterm-256color".into()),
            ("PS1", "$ ".into()),
        ]
    }

    /// Runs `termos ARGS` without a terminal (`ls`, `msg`, …).
    pub fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_termos"));
        cmd.args(args).env_remove("TERMOS").env_remove("TMUX");
        for (k, v) in self.vars() {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn termos")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if self.socket().exists() {
            let _ = self.run(&["kill-server"]);
        }
    }
}

pub struct Termos {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    pub env: Arc<Env>,
}

impl Termos {
    /// Starts `termos` with an isolated config/state/runtime directory and
    /// `/bin/sh` as the shell.
    pub fn start(extra_config: &str) -> Self {
        Self::spawn(Env::new(extra_config), &[])
    }

    /// Another client in the same environment: `termos ARGS` on a new
    /// terminal.
    pub fn client(&self, args: &[&str]) -> Self {
        Self::spawn(Arc::clone(&self.env), args)
    }

    /// Runs `termos ARGS` without a terminal in the same environment.
    pub fn run(&self, args: &[&str]) -> Output {
        self.env.run(args)
    }

    fn spawn(env: Arc<Env>, args: &[&str]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_termos"));
        cmd.args(args);
        cmd.env_remove("TERMOS");
        cmd.env_remove("TMUX");
        for (k, v) in env.vars() {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn termos");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
        let nz = |v: u16| NonZeroU16::new(v).expect("non-zero");
        let shared = Arc::new((
            Mutex::new(Shared {
                parser: vt100::Parser::new(nz(ROWS), nz(COLS), 0),
                raw: Vec::new(),
                generation: 0,
                closed: false,
            }),
            Condvar::new(),
        ));
        let (s2, w2) = (Arc::clone(&shared), Arc::clone(&writer));
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                // Answer the primary device attributes query (crossterm's
                // keyboard-protocol probe) like a VT220 would.
                if buf[..n].windows(3).any(|w| w == b"\x1b[c")
                    && let Ok(mut w) = w2.lock()
                {
                    let _ = w.write_all(b"\x1b[?62;22c");
                    let _ = w.flush();
                }
                let (lock, cv) = &*s2;
                if let Ok(mut s) = lock.lock() {
                    s.parser.process(&buf[..n]);
                    s.raw.extend_from_slice(&buf[..n]);
                    s.generation += 1;
                }
                cv.notify_all();
            }
            let (lock, cv) = &*s2;
            if let Ok(mut s) = lock.lock() {
                s.closed = true;
            }
            cv.notify_all();
        });
        Self {
            shared,
            writer,
            child,
            _master: pair.master,
            env,
        }
    }

    pub fn send(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(bytes).expect("write to termos");
        w.flush().expect("flush");
    }

    pub fn screen(&self) -> String {
        let (lock, _) = &*self.shared;
        lock.lock().expect("lock").parser.screen().contents()
    }

    pub fn generation(&self) -> u64 {
        let (lock, _) = &*self.shared;
        lock.lock().expect("lock").generation
    }

    pub fn raw_output(&self) -> String {
        let (lock, _) = &*self.shared;
        String::from_utf8_lossy(&lock.lock().expect("lock").raw).into_owned()
    }

    /// Waits until `pred(screen)` holds; panics with the screen on timeout.
    pub fn wait_for(&self, what: &str, timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + timeout;
        let (lock, cv) = &*self.shared;
        let mut guard = lock.lock().expect("lock");
        loop {
            let screen = guard.parser.screen().contents();
            if pred(&screen) {
                return screen;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {what}; screen:\n{screen}"
            );
            guard = cv.wait_timeout(guard, deadline - now).expect("wait").0;
        }
    }

    /// Waits until the screen changes after generation `since`; returns the
    /// elapsed time.
    pub fn wait_change(&self, since: u64, timeout: Duration) -> Duration {
        let start = Instant::now();
        let (lock, cv) = &*self.shared;
        let mut guard = lock.lock().expect("lock");
        while guard.generation == since {
            let left = timeout.saturating_sub(start.elapsed());
            assert!(!left.is_zero(), "no output within {timeout:?}");
            guard = cv.wait_timeout(guard, left).expect("wait").0;
        }
        start.elapsed()
    }

    pub fn wait_ready(&self) {
        self.wait_for("the status line", Duration::from_secs(20), |s| {
            s.lines().last().is_some_and(|l| l.starts_with("[main]"))
        });
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "termos did not exit; screen:\n{}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Termos {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
