// SPDX-License-Identifier: GPL-3.0-or-later
//! USB drives (M13d, D-124): the removable block devices Linux has, their
//! labels and file systems from udev's database, where they are mounted,
//! and udev's events when one comes or goes.
//!
//! Everything is read below directories the caller names (`/sys/block`,
//! `/run/udev/data`, `/proc/mounts`' text), so tests use fake trees. The
//! events come from udev's netlink group, which any user may listen to:
//! they arrive once udev has made the device's links, so the label is
//! known by then.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A drive (or one partition of it) that can be mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// `sdb1`.
    pub name: String,
    /// The disk it is on: `sdb` (itself, for a drive with no partitions).
    pub disk: String,
    /// `/dev/sdb1`.
    pub device: PathBuf,
    /// Its file system's label, when it has one.
    pub label: Option<String>,
    /// `vfat`, `exfat`, `ext4`, `ntfs`… as udev found it.
    pub fs: Option<String>,
    pub bytes: u64,
    /// The disk's vendor and model.
    pub model: String,
    /// Where it is mounted.
    pub mount: Option<PathBuf>,
}

impl Drive {
    /// What to call it: its label, else its model and size.
    pub fn title(&self) -> String {
        match &self.label {
            Some(l) => l.clone(),
            None if !self.model.is_empty() => format!("{} ({})", self.model, self.name),
            None => self.name.clone(),
        }
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// The properties udev stored for a device (`E:KEY=VALUE` lines of
/// `/run/udev/data/b<major>:<minor>`).
pub fn udev_properties(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("E:"))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// `\x20` escapes, as udev writes them in labels.
pub fn unescape_udev(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(h) = s.get(i + 2..i + 4)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `\040` escapes, as `/proc/mounts` writes spaces.
fn unescape_mounts(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && let Some(o) = s.get(i + 1..i + 4)
            && let Ok(v) = u8::from_str_radix(o, 8)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Device → mount point, from `/proc/mounts`' text.
pub fn mounts(text: &str) -> HashMap<PathBuf, PathBuf> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let dev = f.next()?;
            let at = f.next()?;
            dev.starts_with("/dev/").then(|| {
                (
                    PathBuf::from(unescape_mounts(dev)),
                    PathBuf::from(unescape_mounts(at)),
                )
            })
        })
        .collect()
}

/// Whether a disk's sysfs folder is a drive the user plugs in: marked
/// removable, or on USB (USB hard disks say they are not removable).
fn pluggable(disk_dir: &Path) -> bool {
    if read(&disk_dir.join("removable")) == "1" {
        return true;
    }
    std::fs::canonicalize(disk_dir).is_ok_and(|p| {
        p.components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with("usb"))
    })
}

/// The removable drives under `sys_block` (`/sys/block`), with what
/// `udev_data` (`/run/udev/data`) knows of them and `mounts_text`
/// (`/proc/mounts`). A drive with partitions shows each; one without (a
/// "superfloppy" stick) shows itself. Disks with no media (an empty card
/// reader) are left out.
pub fn removable(sys_block: &Path, udev_data: &Path, mounts_text: &str) -> Vec<Drive> {
    let mounted = mounts(mounts_text);
    let Ok(entries) = std::fs::read_dir(sys_block) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let disk = e.file_name().to_string_lossy().into_owned();
        let skip = ["loop", "ram", "zram", "sr", "dm-", "md", "nbd", "fd"];
        if skip.iter().any(|p| disk.starts_with(p)) {
            continue;
        }
        let dir = e.path();
        if !pluggable(&dir) {
            continue;
        }
        let model = [
            read(&dir.join("device/vendor")),
            read(&dir.join("device/model")),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let mut parts: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|p| p.path().join("partition").exists())
            .map(|p| (p.file_name().to_string_lossy().into_owned(), p.path()))
            .collect();
        parts.sort();
        if parts.is_empty() {
            parts.push((disk.clone(), dir.clone()));
        }
        for (name, pdir) in parts {
            let bytes = read(&pdir.join("size")).parse::<u64>().unwrap_or(0) * 512;
            if bytes == 0 {
                continue;
            }
            let props = udev_properties(
                &std::fs::read_to_string(udev_data.join(format!("b{}", read(&pdir.join("dev")))))
                    .unwrap_or_default(),
            );
            let device = PathBuf::from(format!("/dev/{name}"));
            let label = props
                .get("ID_FS_LABEL_ENC")
                .map(|l| unescape_udev(l))
                .or_else(|| props.get("ID_FS_LABEL").cloned())
                .filter(|l| !l.is_empty());
            out.push(Drive {
                mount: mounted.get(&device).cloned(),
                label,
                fs: props.get("ID_FS_TYPE").cloned().filter(|t| !t.is_empty()),
                bytes,
                model: model.clone(),
                disk: disk.clone(),
                device,
                name,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The drives plugged in now (Linux).
#[cfg(target_os = "linux")]
pub fn current() -> Vec<Drive> {
    removable(
        Path::new("/sys/block"),
        Path::new("/run/udev/data"),
        &std::fs::read_to_string("/proc/mounts").unwrap_or_default(),
    )
}

/// Elsewhere the system mounts drives itself (Files shows its volumes).
#[cfg(not(target_os = "linux"))]
pub fn current() -> Vec<Drive> {
    Vec::new()
}

/// A device event from udev.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uevent {
    /// `add`, `remove`, `change`…
    pub action: String,
    pub subsystem: String,
    /// `sdb1`, without `/dev/`.
    pub devname: Option<String>,
    /// `disk` or `partition`.
    pub devtype: Option<String>,
    pub properties: HashMap<String, String>,
}

impl Uevent {
    /// A block device came or went.
    pub fn is_block(&self) -> bool {
        self.subsystem == "block" && matches!(self.action.as_str(), "add" | "remove" | "change")
    }
}

/// udev's netlink header: "libudev\0", a magic number, then the header's
/// size and where the properties are (native-endian `u32`s).
const UDEV_PREFIX: &[u8] = b"libudev\0";
const UDEV_MAGIC: u32 = 0xfeed_cafe;

/// Parses a netlink message: udev's (`libudev` header, properties after
/// it) or the kernel's (`add@/devices/…` then properties).
pub fn parse_uevent(msg: &[u8]) -> Option<Uevent> {
    let props: &[u8] = if let Some(rest) = msg.strip_prefix(UDEV_PREFIX) {
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_ne_bytes(rest.get(at..at + 4)?.try_into().ok()?))
        };
        if u32::from_be(word(0)?) != UDEV_MAGIC {
            return None;
        }
        let off = usize::try_from(word(8)?).ok()?;
        let len = usize::try_from(word(12)?).ok()?;
        msg.get(off..off.checked_add(len)?)?
    } else {
        // The kernel's: the first field is ACTION@DEVPATH.
        let first = msg.iter().position(|b| *b == 0)?;
        if !msg[..first].contains(&b'@') {
            return None;
        }
        &msg[first + 1..]
    };
    let mut properties = HashMap::new();
    for field in props.split(|b| *b == 0) {
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        if let Some((k, v)) = field.split_once('=') {
            properties.insert(k.to_owned(), v.to_owned());
        }
    }
    let action = properties.get("ACTION")?.clone();
    let subsystem = properties.get("SUBSYSTEM")?.clone();
    let devname = properties
        .get("DEVNAME")
        .map(|d| d.trim_start_matches("/dev/").to_owned());
    let devtype = properties.get("DEVTYPE").cloned();
    Some(Uevent {
        action,
        subsystem,
        devname,
        devtype,
        properties,
    })
}

/// Listens to udev's events on a thread of its own, which sleeps in the
/// kernel until one comes, and passes each block device's to `on_event`.
/// It stops when `on_event` returns `false`.
#[cfg(target_os = "linux")]
pub fn listen(
    mut on_event: impl FnMut(Uevent) -> bool + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    use rustix::net::netlink::{self, SocketAddrNetlink};
    use rustix::net::{AddressFamily, RecvFlags, SocketFlags, SocketType};
    let fd = rustix::net::socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        Some(netlink::KOBJECT_UEVENT),
    )?;
    // Group 2: udev's messages, sent once it has handled a device.
    rustix::net::bind(&fd, &SocketAddrNetlink::new(0, 2))?;
    std::thread::Builder::new()
        .name("termos-udev".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = match rustix::net::recv(&fd, &mut buf[..], RecvFlags::empty()) {
                    Ok((n, _)) => n,
                    Err(rustix::io::Errno::INTR | rustix::io::Errno::NOBUFS) => continue,
                    Err(e) => {
                        tracing::warn!("udev events stopped: {e}");
                        return;
                    }
                };
                if let Some(ev) = parse_uevent(&buf[..n.min(buf.len())])
                    && ev.is_block()
                    && !on_event(ev)
                {
                    return;
                }
            }
        })
}

/// Elsewhere there are no udev events.
#[cfg(not(target_os = "linux"))]
pub fn listen(
    _on_event: impl FnMut(Uevent) -> bool + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "drive events come from udev, on Linux",
    ))
}

/// A volume the system mounted itself, for Files' sidebar on macOS
/// (`/Volumes`) and Windows (drive letters).
pub fn system_volumes() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut v: Vec<PathBuf> = std::fs::read_dir("/Volumes")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            // The startup disk is linked here too.
            .filter(|p| std::fs::read_link(p).is_err())
            .collect();
        v.sort();
        v
    }
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .map(|l| PathBuf::from(format!("{}:\\", l as char)))
            .filter(|p| p.exists())
            .collect()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn sticks_partitions_and_mounts() {
        let dir = tempfile::tempdir().expect("dir");
        let sys = dir.path().join("block");
        let udev = dir.path().join("udev");
        // A USB stick with one partition, labelled, mounted.
        file(&sys.join("sdb/removable"), "1\n");
        file(&sys.join("sdb/device/vendor"), "SanDisk\n");
        file(&sys.join("sdb/device/model"), "Cruzer\n");
        file(&sys.join("sdb/sdb1/partition"), "1\n");
        file(&sys.join("sdb/sdb1/size"), "62914560\n");
        file(&sys.join("sdb/sdb1/dev"), "8:17\n");
        file(
            &udev.join("b8:17"),
            "S:disk/by-label/MY\\x20STICK\nE:ID_FS_TYPE=vfat\nE:ID_FS_LABEL=MY_STICK\nE:ID_FS_LABEL_ENC=MY\\x20STICK\n",
        );
        // A card reader with no card.
        file(&sys.join("sdc/removable"), "1\n");
        file(&sys.join("sdc/size"), "0\n");
        // The internal disk.
        file(&sys.join("sda/removable"), "0\n");
        file(&sys.join("sda/sda1/partition"), "1\n");
        file(&sys.join("sda/sda1/size"), "1000\n");
        // A stick with no partition table.
        file(&sys.join("sdd/removable"), "1\n");
        file(&sys.join("sdd/size"), "2048\n");
        file(&sys.join("sdd/dev"), "8:48\n");
        file(&udev.join("b8:48"), "E:ID_FS_TYPE=exfat\n");
        let mounts = "/dev/sda2 / ext4 rw 0 0\n/dev/sdb1 /media/MY\\040STICK vfat rw 0 0\n";
        let d = removable(&sys, &udev, mounts);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["sdb1", "sdd"]);
        assert_eq!(d[0].label.as_deref(), Some("MY STICK"));
        assert_eq!(d[0].fs.as_deref(), Some("vfat"));
        assert_eq!(d[0].bytes, 62_914_560 * 512);
        assert_eq!(d[0].model, "SanDisk Cruzer");
        assert_eq!(d[0].mount.as_deref(), Some(Path::new("/media/MY STICK")));
        assert_eq!(d[0].title(), "MY STICK");
        assert_eq!((d[0].disk.as_str(), d[1].disk.as_str()), ("sdb", "sdd"));
        assert_eq!(d[1].label, None);
        assert_eq!(d[1].fs.as_deref(), Some("exfat"));
        assert_eq!(d[1].mount, None);
        assert_eq!(d[1].title(), "sdd");
        assert!(removable(&dir.path().join("missing"), &udev, "").is_empty());
    }

    #[test]
    fn udev_and_kernel_messages() {
        let props = b"ACTION=add\0DEVPATH=/devices/x/sdb/sdb1\0SUBSYSTEM=block\0DEVNAME=/dev/sdb1\0DEVTYPE=partition\0ID_FS_LABEL=STICK\0";
        let mut udev = Vec::new();
        udev.extend_from_slice(b"libudev\0");
        udev.extend_from_slice(&UDEV_MAGIC.to_be_bytes());
        let header = 40u32;
        udev.extend_from_slice(&header.to_ne_bytes());
        udev.extend_from_slice(&header.to_ne_bytes());
        udev.extend_from_slice(&(props.len() as u32).to_ne_bytes());
        udev.resize(header as usize, 0);
        udev.extend_from_slice(props);
        let ev = parse_uevent(&udev).expect("udev's");
        assert_eq!(ev.action, "add");
        assert_eq!(ev.devname.as_deref(), Some("sdb1"));
        assert_eq!(ev.devtype.as_deref(), Some("partition"));
        assert_eq!(
            ev.properties.get("ID_FS_LABEL").map(String::as_str),
            Some("STICK")
        );
        assert!(ev.is_block());

        let kernel = b"remove@/devices/x/sdb\0ACTION=remove\0SUBSYSTEM=block\0DEVNAME=sdb\0";
        let ev = parse_uevent(kernel).expect("the kernel's");
        assert_eq!(
            (ev.action.as_str(), ev.devname.as_deref()),
            ("remove", Some("sdb"))
        );

        // Wrong magic, cut short, or not a uevent at all.
        let mut bad = udev.clone();
        bad[8] ^= 0xff;
        assert_eq!(parse_uevent(&bad), None);
        assert_eq!(parse_uevent(&udev[..30]), None);
        assert_eq!(parse_uevent(b"hello\0ACTION=add\0"), None);
        assert_eq!(parse_uevent(b""), None);
        assert_eq!(unescape_udev("A\\x20B\\x2"), "A B\\x2");
    }
}
