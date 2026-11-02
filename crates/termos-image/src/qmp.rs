// SPDX-License-Identifier: GPL-3.0-or-later
//! A QEMU Machine Protocol client: just what the boot tests need
//! (screenshots, quitting), over the Unix socket QEMU listens on.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

/// The command's JSON, with string arguments (all the boot tests send).
pub fn command(name: &str, args: &[(&str, &str)]) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    if args.is_empty() {
        return format!("{{\"execute\":\"{}\"}}\n", esc(name));
    }
    let fields: Vec<String> = args
        .iter()
        .map(|(k, v)| format!("\"{}\":\"{}\"", esc(k), esc(v)))
        .collect();
    format!(
        "{{\"execute\":\"{}\",\"arguments\":{{{}}}}}\n",
        esc(name),
        fields.join(",")
    )
}

impl Qmp {
    /// Connects and leaves the greeting's capabilities negotiation done.
    pub fn connect(socket: &Path) -> Result<Self, String> {
        let s = UnixStream::connect(socket)
            .map_err(|e| format!("cannot reach QEMU's QMP socket: {e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|e| e.to_string())?;
        let writer = s.try_clone().map_err(|e| e.to_string())?;
        let mut q = Self {
            reader: BufReader::new(s),
            writer,
        };
        let mut greeting = String::new();
        q.reader
            .read_line(&mut greeting)
            .map_err(|e| e.to_string())?;
        if !greeting.contains("QMP") {
            return Err(format!("not a QMP greeting: {greeting}"));
        }
        q.run("qmp_capabilities", &[])?;
        Ok(q)
    }

    /// Runs a command; its reply line (events before it are skipped).
    pub fn run(&mut self, name: &str, args: &[(&str, &str)]) -> Result<String, String> {
        self.writer
            .write_all(command(name, args).as_bytes())
            .map_err(|e| format!("QMP {name}: {e}"))?;
        loop {
            let mut line = String::new();
            if self
                .reader
                .read_line(&mut line)
                .map_err(|e| format!("QMP {name}: {e}"))?
                == 0
            {
                return Err(format!("QMP {name}: QEMU closed the socket"));
            }
            if line.contains("\"error\"") {
                return Err(format!("QMP {name}: {}", line.trim()));
            }
            if line.contains("\"return\"") {
                return Ok(line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_json() {
        assert_eq!(command("quit", &[]), "{\"execute\":\"quit\"}\n");
        assert_eq!(
            command("screendump", &[("filename", "/tmp/a \"b\".ppm")]),
            "{\"execute\":\"screendump\",\"arguments\":{\"filename\":\"/tmp/a \\\"b\\\".ppm\"}}\n"
        );
    }

    #[test]
    fn a_session_with_a_fake_qemu() {
        let dir = std::env::temp_dir().join(format!("termos-qmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let sock = dir.join("qmp.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().expect("accept");
            let mut w = s.try_clone().expect("clone");
            let mut r = BufReader::new(s);
            w.write_all(b"{\"QMP\": {\"version\": {}}}\n")
                .expect("greet");
            let mut seen = Vec::new();
            for reply in [
                "{\"return\": {}}\n",
                "{\"event\": \"X\"}\n{\"return\": {}}\n",
                "{\"error\": {\"desc\": \"no\"}}\n",
            ] {
                let mut line = String::new();
                r.read_line(&mut line).expect("read");
                seen.push(line);
                w.write_all(reply.as_bytes()).expect("reply");
            }
            seen
        });
        let mut q = Qmp::connect(&sock).expect("connect");
        assert!(
            q.run("screendump", &[("filename", "x.ppm")]).is_ok(),
            "an event is skipped"
        );
        assert!(q.run("bad", &[]).expect_err("error").contains("no"));
        let seen = server.join().expect("server");
        assert!(seen[0].contains("qmp_capabilities") && seen[1].contains("screendump"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
