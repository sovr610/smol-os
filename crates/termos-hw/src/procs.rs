// SPDX-License-Identifier: GPL-3.0-or-later
//! The process manager's data and actions (M13c, D-122): every process
//! with its parent, user, start, state, threads and command line, the TCP
//! ports it listens on (Linux), and signals and priorities.

use std::collections::HashMap;

/// One process, for the process manager.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    /// The user it runs as (Linux; `None` elsewhere or when unknown).
    pub user: Option<String>,
    /// Percent of one CPU.
    pub cpu: f32,
    /// Resident bytes.
    pub memory: u64,
    /// Seconds it has run.
    pub run_time: u64,
    /// Running, sleeping, stopped… as the system says.
    pub state: String,
    /// Its threads (Linux).
    pub threads: Option<u32>,
    /// Its command line, or its name when the system gives none.
    pub command: String,
    /// TCP ports it listens on (Linux), filled when asked for.
    pub ports: Vec<u16>,
}

/// What can be done to a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// SIGTERM; on Windows, TerminateProcess.
    End,
    /// SIGKILL.
    Kill,
    /// SIGSTOP (Unix).
    Pause,
    /// SIGCONT (Unix).
    Resume,
}

impl Action {
    /// What it is called in questions: "end", "kill", …
    pub fn verb(self) -> &'static str {
        match self {
            Action::End => "end",
            Action::Kill => "kill",
            Action::Pause => "pause",
            Action::Resume => "resume",
        }
    }
}

/// Whether TermOS refuses to touch `pid`: the system's first processes and
/// TermOS itself.
fn protected(pid: u32) -> bool {
    pid <= 1 || pid == std::process::id()
}

/// Sends `action` to process `pid`. Another user's process gives the
/// system's refusal; there is no sudo from here.
pub fn act(pid: u32, action: Action) -> Result<(), String> {
    use sysinfo::{Pid, ProcessesToUpdate, Signal, System};
    if protected(pid) {
        return Err(format!("process {pid} is not one TermOS touches"));
    }
    if cfg!(windows) && matches!(action, Action::Pause | Action::Resume) {
        return Err("Windows cannot pause a program from here".into());
    }
    let target = Pid::from_u32(pid);
    let mut system = System::new().map_err(|e| format!("cannot read processes: {e}"))?;
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    let process = system
        .process(target)
        .ok_or_else(|| format!("no process {pid}"))?;
    let signal = match action {
        Action::End => Signal::Term,
        Action::Kill => Signal::Kill,
        Action::Pause => Signal::Stop,
        Action::Resume => Signal::Continue,
    };
    let sent = match process.kill_with(signal) {
        Some(sent) => sent,
        // Windows knows no signals: ending and killing terminate.
        None if matches!(action, Action::End | Action::Kill) => process.kill(),
        None => false,
    };
    if sent {
        Ok(())
    } else {
        Err(format!(
            "cannot {} process {pid} ({}): not allowed",
            action.verb(),
            process.name().to_string_lossy()
        ))
    }
}

/// Changes process `pid`'s priority by `delta` (positive: nicer, lower
/// priority); its new niceness. Raising a priority needs privileges the
/// desktop does not have, and the system says so.
#[cfg(unix)]
pub fn renice(pid: u32, delta: i32) -> Result<i32, String> {
    use rustix::process::{Pid, getpriority_process, setpriority_process};
    if protected(pid) {
        return Err(format!("process {pid} is not one TermOS touches"));
    }
    let p = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| format!("no process {pid}"))?;
    let now = getpriority_process(Some(p)).map_err(|e| format!("process {pid}: {e}"))?;
    let next = (now + delta).clamp(-20, 19);
    setpriority_process(Some(p), next).map_err(|e| format!("cannot change process {pid}: {e}"))?;
    Ok(next)
}

/// Windows: priorities are not changed from here.
#[cfg(not(unix))]
pub fn renice(_pid: u32, _delta: i32) -> Result<i32, String> {
    Err("priorities are not changed from TermOS on Windows".into())
}

/// The user names of the system's users, by id (`/etc/passwd`).
#[cfg(target_os = "linux")]
fn users() -> HashMap<u32, String> {
    std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let uid = f.nth(1)?.parse().ok()?;
            Some((uid, name.to_owned()))
        })
        .collect()
}

/// A process's user id and threads, from `/proc/PID/status`.
#[cfg(target_os = "linux")]
fn status_of(pid: u32) -> (Option<u32>, Option<u32>) {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    (field("Uid:"), field("Threads:"))
}

/// The TCP ports listened on, by socket inode, from `/proc/net/tcp` and
/// `tcp6` text.
pub fn listening_inodes(tables: &[&str]) -> HashMap<u64, u16> {
    let mut out = HashMap::new();
    for table in tables {
        for line in table.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // sl local rem st tx:rx tr:tm retrnsmt uid timeout inode
            let (Some(local), Some(state), Some(inode)) = (cols.get(1), cols.get(3), cols.get(9))
            else {
                continue;
            };
            if *state != "0A" {
                continue;
            }
            let Some(port) = local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
            else {
                continue;
            };
            if let Ok(inode) = inode.parse::<u64>() {
                out.insert(inode, port);
            }
        }
    }
    out
}

/// The ports each of `pids` listens on (Linux): its sockets matched with
/// the listening ones.
#[cfg(target_os = "linux")]
pub fn ports(pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    let tables: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    let refs: Vec<&str> = tables.iter().map(String::as_str).collect();
    let listening = listening_inodes(&refs);
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    if listening.is_empty() {
        return out;
    }
    for pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let t = target.to_string_lossy();
            let Some(inode) = t
                .strip_prefix("socket:[")
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|n| n.parse::<u64>().ok())
            else {
                continue;
            };
            if let Some(port) = listening.get(&inode) {
                let v = out.entry(*pid).or_default();
                if !v.contains(port) {
                    v.push(*port);
                }
            }
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
    }
    out
}

/// Elsewhere, ports are not read.
#[cfg(not(target_os = "linux"))]
pub fn ports(_pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    HashMap::new()
}

/// Whether [`ports`] can say anything here.
pub const PORTS_AVAILABLE: bool = cfg!(target_os = "linux");

/// Every process of `system` (refreshed by the caller), with ports when
/// asked.
pub(crate) fn table(system: &sysinfo::System, with_ports: bool) -> Vec<ProcessInfo> {
    #[cfg(target_os = "linux")]
    let users = users();
    let mut out: Vec<ProcessInfo> = system
        .processes()
        .values()
        .filter(|p| p.thread_kind() != Some(sysinfo::ThreadKind::Userland))
        .map(|p| {
            let pid = p.pid().as_u32();
            let cmd: Vec<String> = p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let name = p.name().to_string_lossy().into_owned();
            #[cfg(target_os = "linux")]
            let (uid, threads) = status_of(pid);
            #[cfg(target_os = "linux")]
            let user = uid.map(|u| users.get(&u).cloned().unwrap_or_else(|| u.to_string()));
            #[cfg(not(target_os = "linux"))]
            let (user, threads) = (None, None);
            ProcessInfo {
                pid,
                parent: p.parent().map(|pp| pp.as_u32()),
                user,
                cpu: p.cpu_usage(),
                memory: p.memory(),
                run_time: p.run_time(),
                state: p.status().to_string(),
                threads,
                command: if cmd.is_empty() {
                    name.clone()
                } else {
                    cmd.join(" ")
                },
                name,
                ports: Vec::new(),
            }
        })
        .collect();
    if with_ports && PORTS_AVAILABLE {
        let pids: Vec<u32> = out.iter().map(|p| p.pid).collect();
        let ports = ports(&pids);
        for p in &mut out {
            if let Some(v) = ports.get(&p.pid) {
                p.ports.clone_from(v);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_sockets_are_read_from_proc_net_tcp() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41234 1 0\n\
   1: 0100007F:0277 0100007F:A3B2 01 00000000:00000000 00:00000000 00000000  1000        0 41299 1 0\n";
        let six = "  sl  local_address                         remote_address                        st\n\
   0: 00000000000000000000000000000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 777 1\n";
        let got = listening_inodes(&[table, six]);
        assert_eq!(got.get(&41234), Some(&8080));
        assert_eq!(got.get(&777), Some(&80));
        assert_eq!(got.get(&41299), None, "an established connection");
    }

    #[test]
    fn termos_and_init_are_left_alone() {
        assert!(act(1, Action::Kill).is_err());
        assert!(act(std::process::id(), Action::End).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_child_is_paused_resumed_and_ended() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let pid = child.id();
        act(pid, Action::Pause).expect("paused");
        act(pid, Action::Resume).expect("resumed");
        // From wherever it started (macOS runners start below 0).
        let raw =
            rustix::process::Pid::from_raw(i32::try_from(pid).expect("a pid")).expect("a pid");
        let before = rustix::process::getpriority_process(Some(raw)).expect("its priority");
        let nicer = renice(pid, 1).expect("niced");
        assert_eq!(nicer, (before + 1).clamp(-20, 19));
        assert_eq!(rustix::process::getpriority_process(Some(raw)), Ok(nicer));
        act(pid, Action::End).expect("ended");
        let status = child.wait().expect("waited");
        assert!(!status.success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_listener_shows_its_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().expect("addr").port();
        let me = std::process::id();
        let got = ports(&[me]);
        assert!(got.get(&me).is_some_and(|v| v.contains(&port)), "{got:?}");
    }
}
