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
use std::sync::{Arc, Mutex};
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
    /// An app pane's background work finished: draw it again. (App panes
    /// have no PTY; their wakeups travel the same way to the desktop.)
    Wake(PaneId),
}

/// How to start a pane's program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (added to the inherited environment).
    pub env: Vec<(String, String)>,
    /// Inherited variables to leave out (an agent's commands do not get
    /// the server's keys or sockets).
    pub env_remove: Vec<String>,
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
            env_remove: Vec::new(),
            cols,
            rows,
        }
    }
}

/// Output chunks are at most this large.
const READ_CHUNK: usize = 64 * 1024;

/// The PTY master, shared with the Windows waiter thread, which closes it
/// once the child exits.
type SharedMaster = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;

/// A running child on a PTY.
pub struct PtyPane {
    id: PaneId,
    master: SharedMaster,
    input: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
    /// The last look at the foreground program, reused for a second: tabs
    /// ask on every frame, and asking the OS costs more here than reading
    /// Linux's /proc.
    #[cfg(any(target_os = "macos", windows))]
    foreground: std::sync::Mutex<Option<(std::time::Instant, Option<os::Foreground>)>>,
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
        for k in &spec.env_remove {
            cmd.env_remove(k);
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
        let master: SharedMaster = Arc::new(Mutex::new(Some(pair.master)));

        // How the reader learns the exit code once output ends. On Unix
        // output ends when the child (and everything holding the terminal)
        // exits, then the reader waits for the child. A ConPTY keeps its
        // output open until the pseudo-console is closed: a waiter thread
        // waits for the child, then closes it, which ends the output.
        #[cfg(windows)]
        let exit_code = {
            let (tx, rx) = std_mpsc::channel::<Option<u32>>();
            let master = Arc::clone(&master);
            thread::Builder::new()
                .name(format!("pty-wait-{}", id.0))
                .spawn(move || {
                    let code = child.wait().ok().map(|s| s.exit_code());
                    let _ = tx.send(code);
                    if let Ok(mut m) = master.lock() {
                        m.take();
                    }
                })?;
            move || rx.recv().ok().flatten()
        };
        #[cfg(not(windows))]
        let mut exit_code = move || child.wait().ok().map(|s| s.exit_code());

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
                let _ = events.blocking_send(PtyEvent::Exited(id, exit_code()));
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
            master,
            input,
            killer,
            pid,
            #[cfg(any(target_os = "macos", windows))]
            foreground: std::sync::Mutex::new(None),
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

    /// Resizes the PTY (the child receives SIGWINCH). A pane whose child
    /// has exited has nothing left to resize.
    /// Resizes the terminal, telling the program the pixel size too when
    /// the cell size is known (image tools size their output by it).
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        cell_px: Option<termos_render::caps::PxSize>,
    ) -> io::Result<()> {
        let px = |n: u16, cell: Option<u32>| {
            cell.map_or(0, |c| u16::try_from(u32::from(n) * c).unwrap_or(u16::MAX))
        };
        let master = self
            .master
            .lock()
            .map_err(|_| io_err("pty lock poisoned"))?;
        match master.as_ref() {
            Some(m) => m
                .resize(PtySize {
                    rows: rows.max(1),
                    cols: cols.max(1),
                    pixel_width: px(cols, cell_px.map(|c| c.width)),
                    pixel_height: px(rows, cell_px.map(|c| c.height)),
                })
                .map_err(io_err),
            None => Ok(()),
        }
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
            .lock()
            .ok()
            .and_then(|m| m.as_ref()?.process_group_leader())
            .and_then(|p| u32::try_from(p).ok());
        #[cfg(not(unix))]
        let leader = None;
        leader.or(self.pid)
    }

    /// Name of the foreground program.
    pub fn foreground_name(&self) -> Option<String> {
        #[cfg(any(target_os = "macos", windows))]
        return self.foreground_info()?.name;
        #[cfg(not(any(target_os = "macos", windows)))]
        proc_comm(self.foreground_pid()?)
    }

    /// Working directory of the foreground program.
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        #[cfg(any(target_os = "macos", windows))]
        return self.foreground_info()?.cwd;
        #[cfg(not(any(target_os = "macos", windows)))]
        proc_cwd(self.foreground_pid()?)
    }

    /// The command line of the foreground program when it is not the
    /// pane's own program, e.g. `vim notes.md` started from the shell
    /// (`None` at the shell prompt).
    pub fn foreground_command(&self) -> Option<Vec<String>> {
        #[cfg(any(target_os = "macos", windows))]
        {
            let fg = self.foreground_info()?;
            if Some(fg.pid) == self.pid {
                return None;
            }
            fg.argv
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let pid = self.foreground_pid()?;
            if Some(pid) == self.pid {
                return None;
            }
            // Between fork and exec it is not the user's program yet.
            proc_comm(pid)?;
            proc_cmdline(pid)
        }
    }

    /// The foreground program as the OS reports it, looked at most once a
    /// second (no timer: a stale look is replaced when asked for).
    #[cfg(any(target_os = "macos", windows))]
    fn foreground_info(&self) -> Option<os::Foreground> {
        let now = std::time::Instant::now();
        let mut cache = self.foreground.lock().ok()?;
        if let Some((at, fg)) = cache.as_ref()
            && now.duration_since(*at) < std::time::Duration::from_secs(1)
        {
            return fg.clone();
        }
        // macOS: the terminal's foreground process group, as on Linux.
        // Windows has none: the newest program started under the shell.
        #[cfg(target_os = "macos")]
        let fg = self.foreground_pid().and_then(os::describe);
        #[cfg(windows)]
        let fg = self.pid.and_then(os::newest_below);
        *cache = Some((now, fg.clone()));
        fg
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

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
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

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn proc_cmdline(_pid: u32) -> Option<Vec<String>> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn proc_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// The foreground program on macOS and Windows, through `sysinfo` (which
/// asks libproc and sysctl on macOS, and the process list and each
/// process's parameters on Windows).
#[cfg(any(target_os = "macos", windows))]
mod os {
    use std::path::PathBuf;

    use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Foreground {
        pub pid: u32,
        pub name: Option<String>,
        pub cwd: Option<PathBuf>,
        pub argv: Option<Vec<String>>,
    }

    fn details() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
    }

    fn foreground(pid: u32, p: &Process) -> Foreground {
        let name = p.name().to_string_lossy();
        let name = match name.len().checked_sub(4) {
            Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => {
                name[..i].to_owned()
            }
            _ => name.into_owned(),
        };
        let argv: Vec<String> = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        Foreground {
            pid,
            name: (!name.is_empty()).then_some(name),
            cwd: p.cwd().map(std::path::Path::to_path_buf),
            argv: (!argv.is_empty()).then_some(argv),
        }
    }

    /// The process `pid`.
    #[cfg(target_os = "macos")]
    pub(super) fn describe(pid: u32) -> Option<Foreground> {
        let mut sys = System::new().ok()?;
        let id = Pid::from_u32(pid);
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[id]), true, details());
        sys.process(id).map(|p| foreground(pid, p))
    }

    /// The newest process below `shell` (not a console host): the
    /// program the user started last, or the shell itself.
    #[cfg(windows)]
    pub(super) fn newest_below(shell: u32) -> Option<Foreground> {
        use std::collections::HashMap;

        let mut sys = System::new().ok()?;
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, p) in sys.processes() {
            if let Some(parent) = p.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }
        let root = Pid::from_u32(shell);
        let (mut best, mut best_start) = (root, 0);
        let mut stack = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(parent) = stack.pop() {
            for &pid in children.get(&parent).into_iter().flatten() {
                let Some(p) = sys.process(pid) else { continue };
                let name = p.name().to_string_lossy().to_ascii_lowercase();
                if !seen.insert(pid) || name == "conhost.exe" || name == "openconsole.exe" {
                    continue;
                }
                stack.push(pid);
                if p.start_time() >= best_start {
                    (best, best_start) = (pid, p.start_time());
                }
            }
        }
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[best]), false, details());
        sys.process(best).map(|p| foreground(best.as_u32(), p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Collects output until the program exits. Answers cursor-position
    /// queries the way a terminal would: ConPTY asks one (`CSI 6n`) when it
    /// starts and holds back all output until it gets the answer.
    async fn collect(pane: &PtyPane, rx: &mut mpsc::Receiver<PtyEvent>) -> (Vec<u8>, Option<u32>) {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                Ok(Some(PtyEvent::Output(_, bytes))) => {
                    if bytes.windows(4).any(|w| w == b"\x1b[6n") {
                        pane.write(b"\x1b[1;1R".to_vec());
                    }
                    out.extend(bytes);
                }
                Ok(Some(PtyEvent::Exited(_, code))) => return (out, code),
                Ok(Some(PtyEvent::Wake(_))) => {}
                Ok(None) | Err(_) => {
                    panic!("pty did not exit; got {:?}", String::from_utf8_lossy(&out))
                }
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_program_and_reports_exit() {
        let (tx, mut rx) = mpsc::channel(16);
        let spec = SpawnSpec {
            args: vec![
                "-c".into(),
                "printf 'hi %s' \"$TERMOS_TEST\"; exit 3".into(),
            ],
            env: vec![("TERMOS_TEST".into(), "there".into())],
            env_remove: Vec::new(),
            ..SpawnSpec::shell("/bin/sh", 40, 5)
        };
        let pane = PtyPane::spawn(PaneId(1), &spec, tx).expect("spawn");
        let (out, code) = collect(&pane, &mut rx).await;
        assert!(String::from_utf8_lossy(&out).contains("hi there"));
        assert_eq!(code, Some(3));
    }

    #[cfg(unix)]
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
        let (out, code) = collect(&pane, &mut rx).await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("7 50"), "{text:?}");
        assert!(text.contains("got:hello"), "{text:?}");
        assert_eq!(code, Some(0));
    }

    #[cfg(unix)]
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
        assert_eq!(pane.foreground_name().as_deref(), Some("sleep"));
        assert!(pane.foreground_cwd().is_some_and(|d| d.is_dir()));
    }

    /// Windows has no foreground process group: the newest program under
    /// the shell is the one shown and restored.
    #[cfg(windows)]
    #[tokio::test]
    async fn reports_the_newest_program_under_the_shell() {
        let (tx, mut rx) = mpsc::channel(64);
        let spec = SpawnSpec::shell("cmd.exe", 60, 5);
        let pane = PtyPane::spawn(PaneId(4), &spec, tx).expect("spawn");
        // Answer ConPTY's cursor query, and keep the channel drained.
        let writer = pane.input.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if let PtyEvent::Output(_, bytes) = ev
                    && bytes.windows(4).any(|w| w == b"\x1b[6n")
                {
                    let _ = writer.send(b"\x1b[1;1R".to_vec());
                }
            }
        });
        pane.write(b"ping -n 60 127.0.0.1\r".to_vec());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(argv) = pane.foreground_command() {
                assert!(
                    argv.first()
                        .is_some_and(|a| a.to_ascii_lowercase().contains("ping")),
                    "{argv:?}"
                );
                assert!(argv.iter().any(|a| a == "127.0.0.1"), "{argv:?}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ping never showed up under the shell"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            pane.foreground_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("ping")),
            "{:?}",
            pane.foreground_name()
        );
        assert!(pane.foreground_cwd().is_some_and(|d| d.is_dir()));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn conpty_runs_a_program_and_reports_exit() {
        let (tx, mut rx) = mpsc::channel(64);
        let spec = SpawnSpec {
            args: vec!["/c".into(), "echo hi %TERMOS_TEST%& exit 3".into()],
            env: vec![("TERMOS_TEST".into(), "there".into())],
            env_remove: Vec::new(),
            ..SpawnSpec::shell("cmd.exe", 40, 5)
        };
        let pane = PtyPane::spawn(PaneId(11), &spec, tx).expect("spawn");
        let (out, code) = collect(&pane, &mut rx).await;
        assert!(String::from_utf8_lossy(&out).contains("hi there"));
        assert_eq!(code, Some(3));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn conpty_input_reaches_the_child() {
        let (tx, mut rx) = mpsc::channel(64);
        let spec = SpawnSpec {
            args: vec![
                "/v:on".into(),
                "/c".into(),
                "set /p line=& echo got:!line!".into(),
            ],
            ..SpawnSpec::shell("cmd.exe", 50, 7)
        };
        let pane = PtyPane::spawn(PaneId(12), &spec, tx).expect("spawn");
        pane.write(b"hello\r".to_vec());
        let (out, code) = collect(&pane, &mut rx).await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("got:hello"), "{text:?}");
        assert_eq!(code, Some(0));
        pane.resize(60, 10, None)
            .expect("resizing after exit is harmless");
    }
}
