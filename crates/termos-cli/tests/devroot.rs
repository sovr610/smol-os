// SPDX-License-Identifier: GPL-3.0-or-later
//! The dev rootfs (M5b, D-110) with the real `termos`: bubblewrap in a
//! user namespace with the user's subordinate IDs, on a stand-in rootfs
//! made of the host's own programs and a recording `apt-get` (CI's
//! `devroot` job makes the real Debian one), and Podman through a
//! stand-in that records its arguments.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Output;

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Why the bubblewrap way cannot run here, if it cannot.
#[cfg(target_os = "linux")]
fn not_ready() -> Option<String> {
    use termos_devkit::devroot;
    let tools = match devroot::tools(std::env::var_os("PATH").as_ref()) {
        Ok(t) => t,
        Err(e) => return Some(e.to_string()),
    };
    let ids = match devroot::ids() {
        Ok(i) => i,
        Err(e) => return Some(e.to_string()),
    };
    let out = std::process::Command::new(&tools.unshare)
        .args(devroot::unshare_args(&ids))
        .arg("true")
        .output()
        .expect("unshare");
    (!out.status.success()).then(|| String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A rootfs of the host's own `sh` and a few tools with their libraries,
/// an `apt-get` that records its arguments and two installed packages.
#[cfg(target_os = "linux")]
fn stand_in_rootfs(dir: &Path) -> (std::path::PathBuf, String) {
    let tree = dir.join("tree");
    for d in [
        "bin",
        "usr/bin",
        "etc",
        "root",
        "tmp",
        "var/log",
        "var/lib/dpkg",
    ] {
        std::fs::create_dir_all(tree.join(d)).expect("mkdir");
    }
    let which = |p: &str| {
        termos_devkit::mise::which(p, std::env::var_os("PATH").as_ref())
            .unwrap_or_else(|| panic!("no {p} here"))
    };
    for (name, program) in [
        ("sh", "sh"),
        ("id", "id"),
        ("cat", "cat"),
        ("touch", "touch"),
        ("ls", "ls"),
    ] {
        let src = std::fs::canonicalize(which(program)).expect("canonicalize");
        std::fs::copy(&src, tree.join("bin").join(name)).expect("copy");
        let ldd = std::process::Command::new("ldd")
            .arg(&src)
            .output()
            .expect("ldd");
        for lib in String::from_utf8_lossy(&ldd.stdout)
            .split_whitespace()
            .filter(|w| w.starts_with('/'))
        {
            let dest = tree.join(lib.trim_start_matches('/'));
            std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
            std::fs::copy(lib, &dest).expect("copy library");
        }
    }
    script(
        &tree.join("usr/bin/apt-get"),
        "#!/bin/sh\necho \"apt-get $*\" >> /var/log/apt-calls\n",
    );
    script(
        &tree.join("usr/bin/apt-cache"),
        "#!/bin/sh\necho 'hello - example package based on GNU hello'\necho 'hello-traditional - the classic greeting'\n",
    );
    std::fs::write(
        tree.join("var/lib/dpkg/status"),
        "Package: base-files\nStatus: install ok installed\nVersion: 13.8\nDescription: Debian base system miscellaneous files\n\n\
         Package: bash\nStatus: install ok installed\nVersion: 5.2.37-2\nDescription: GNU Bourne Again SHell\n",
    )
    .expect("status");
    let archive = dir.join("rootfs.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .args(["--owner=0", "--group=0", "-C"])
        .arg(&tree)
        .arg(".")
        .status()
        .expect("tar");
    assert!(status.success());
    let sum = termos_devkit::mise::sha256_file(&archive).expect("sum");
    (archive, sum)
}

#[cfg(target_os = "linux")]
#[test]
fn the_dev_rootfs_is_created_entered_updated_and_removed() {
    if let Some(why) = not_ready() {
        assert!(
            std::env::var_os("TERMOS_REQUIRE_DEVROOT").is_none(),
            "the dev rootfs cannot run here: {why}"
        );
        eprintln!("skipped: the dev rootfs cannot run here: {why}");
        return;
    }
    let env = common::Env::new("");
    let work = tempfile::tempdir().expect("tempdir");
    let (archive, sum) = stand_in_rootfs(work.path());
    let archive = archive.to_string_lossy().into_owned();
    let data = env.path().join("data/termos/devroot");

    let status = ok(&env.run(&["devroot", "status"]), "status");
    assert!(status.contains("No dev rootfs"), "{status}");
    assert!(status.contains("bubblewrap: ready"), "{status}");

    // A file that is not what was asked for is not unpacked.
    let wrong = env.run(&[
        "devroot",
        "create",
        "--from",
        &archive,
        "--sha256",
        &"0".repeat(64),
    ]);
    assert!(!wrong.status.success());
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("checksum mismatch"),
        "{wrong:?}"
    );
    assert!(!data.join("rootfs").exists());

    let made = ok(
        &env.run(&["devroot", "create", "--from", &archive, "--sha256", &sum]),
        "create",
    );
    assert!(made.contains("Ready"), "{made}");
    let again = env.run(&["devroot", "create", "--from", &archive, "--sha256", &sum]);
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("there is a dev rootfs already"),
        "{again:?}"
    );
    let status = ok(&env.run(&["devroot", "status"]), "status");
    assert!(status.contains("packages: 2 installed"), "{status}");
    assert!(status.contains("updated:  never"), "{status}");

    // Root inside; the project folder is the host's, and what is made in
    // it belongs to the user outside.
    let project = env.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir");
    let inside = ok(
        &env.run_in(
            &project,
            &[
                "devroot",
                "enter",
                "--",
                "/bin/sh",
                "-c",
                "id -u; echo $HOME; echo $TERMOS_DEVROOT; touch made-inside; cat /var/log/apt-calls; ls -ld /tmp",
            ],
        ),
        "enter",
    );
    let lines: Vec<&str> = inside.lines().collect();
    assert_eq!(&lines[..3], ["0", "/root", "1"], "{inside}");
    assert!(
        inside.contains("apt-get update"),
        "create fetched the lists: {inside}"
    );
    // apt checks signatures as its own user, in /tmp.
    assert!(inside.contains("drwxrwxrwt"), "/tmp for everyone: {inside}");
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::metadata(project.join("made-inside")).expect("made inside");
        let me = std::fs::metadata(env.path()).expect("home");
        assert_eq!(meta.uid(), me.uid());
    }
    let code = env.run(&["devroot", "enter", "--", "/bin/sh", "-c", "exit 7"]);
    assert_eq!(code.status.code(), Some(7));

    // The package manager's source: listed from dpkg's database, searched
    // and installed with apt inside.
    let listed = ok(&env.run(&["pkg", "list", "devroot"]), "pkg list");
    assert!(listed.contains("bash"), "{listed}");
    assert!(listed.contains("5.2.37-2"), "{listed}");
    let found = ok(
        &env.run(&["pkg", "search", "devroot", "hello"]),
        "pkg search",
    );
    assert!(found.contains("hello-traditional"), "{found}");
    let installed = ok(
        &env.run(&["pkg", "install", "devroot", "hello"]),
        "pkg install",
    );
    assert!(
        installed.contains("$ apt-get install hello (in the dev rootfs)"),
        "{installed}"
    );
    let calls = std::fs::read_to_string(data.join("rootfs/var/log/apt-calls")).expect("calls");
    assert!(calls.contains("apt-get install hello"), "{calls}");
    let bad = env.run(&["pkg", "install", "devroot", "-y"]);
    assert!(!bad.status.success(), "an option is not a package");

    let updated = ok(&env.run(&["devroot", "update"]), "update");
    assert!(updated.contains("Up to date"), "{updated}");
    let calls = std::fs::read_to_string(data.join("rootfs/var/log/apt-calls")).expect("calls");
    assert!(calls.contains("-y"), "{calls}");
    assert!(calls.trim_end().ends_with("upgrade"), "{calls}");
    let status = ok(&env.run(&["devroot", "status"]), "status");
    assert!(!status.contains("updated:  never"), "{status}");

    let refused = env.run(&["devroot", "remove"]);
    assert!(!refused.status.success(), "asks without a terminal");
    let gone = ok(&env.run(&["devroot", "remove", "--yes"]), "remove");
    assert!(gone.contains("Deleted"), "{gone}");
    assert!(!data.exists());
    let none = env.run(&["devroot", "enter"]);
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("no dev rootfs yet"),
        "{none:?}"
    );
}

#[test]
fn podman_runs_the_same_image_in_a_container() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    script(
        &bin.path().join("podman"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1 $2\" in\n  \"container exists\") exit 1 ;;\n  \"container inspect\") echo running ;;\nesac\nexit 0\n",
            calls.display()
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars("", vec![("PATH", path.into())]);
    let home = env.path().display().to_string();

    let made = ok(&env.run(&["devroot", "create", "--podman"]), "create");
    assert!(made.contains("Ready"), "{made}");
    let log = std::fs::read_to_string(&calls).expect("calls");
    let image = termos_devkit::devroot::podman_image();
    assert!(log.contains(&format!("pull {image}")), "{log}");
    assert!(
        log.contains(&format!("--volume {home}:{home} {image} sleep infinity")),
        "{log}"
    );
    assert!(log.contains("start termos-devroot"), "{log}");
    assert!(
        log.contains(&format!(
            "exec --interactive --workdir {home} --env HOME=/root"
        )),
        "{log}"
    );
    assert!(
        log.contains("termos-devroot /bin/sh -c apt-get update"),
        "{log}"
    );

    let status = ok(&env.run(&["devroot", "status"]), "status");
    assert!(status.contains("(Podman)"), "{status}");
    assert!(
        status.contains("container: termos-devroot (running)"),
        "{status}"
    );

    ok(&env.run(&["devroot", "enter", "--", "echo", "hi"]), "enter");
    let log = std::fs::read_to_string(&calls).expect("calls");
    assert!(log.trim_end().ends_with("termos-devroot echo hi"), "{log}");
    assert!(!log.contains("--tty"), "no terminal here: {log}");

    ok(&env.run(&["devroot", "stop"]), "stop");
    let gone = ok(&env.run(&["devroot", "remove", "--yes"]), "remove");
    assert!(gone.contains("podman rmi"), "{gone}");
    let log = std::fs::read_to_string(&calls).expect("calls");
    assert!(log.contains("stop termos-devroot"), "{log}");
    assert!(log.contains("rm --force termos-devroot"), "{log}");
    let status = ok(&env.run(&["devroot", "status"]), "status");
    assert!(status.contains("No dev rootfs"), "{status}");
}

/// mise as the package manager's source, through a stand-in that answers
/// like mise and records what it was asked to do.
#[test]
fn pkg_lists_searches_and_installs_mise_tools() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let mise = bin.path().join("mise");
    script(
        &mise,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n\
             --version) echo '2026.10.14 linux-x64 (2026-10-18)';;\n\
             ls) echo '{{\"go\": [{{\"version\": \"1.25.3\", \"installed\": true, \"active\": true}}]}}';;\n\
             registry) printf 'jq   aqua:jqlang/jq\\njqp  aqua:noahgorstein/jqp\\nripgrep aqua:BurntSushi/ripgrep\\n';;\n\
             *) echo \"$*\" >> '{}';;\n\
             esac\n",
            calls.display()
        ),
    );
    let env = common::Env::with_vars("", vec![("TERMOS_MISE", mise)]);
    let sources = ok(&env.run(&["pkg", "sources"]), "sources");
    assert!(
        sources
            .lines()
            .any(|l| l.starts_with("mise") && l.ends_with("ready")),
        "{sources}"
    );
    assert!(sources.contains("No dev rootfs yet"), "{sources}");
    let listed = ok(&env.run(&["pkg", "list", "mise"]), "list");
    assert!(
        listed.contains("go") && listed.contains("1.25.3"),
        "{listed}"
    );
    let found = ok(&env.run(&["pkg", "search", "mise", "jq"]), "search");
    assert!(
        found.contains("jqp") && !found.contains("ripgrep"),
        "{found}"
    );
    ok(&env.run(&["pkg", "install", "mise", "jq"]), "install");
    ok(&env.run(&["pkg", "remove", "mise", "jq"]), "remove");
    ok(&env.run(&["pkg", "upgrade", "mise"]), "upgrade");
    let log = std::fs::read_to_string(&calls).expect("calls");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["use --global jq@latest", "unuse --global jq", "upgrade"]
    );
    let unknown = env.run(&["pkg", "list", "apt"]);
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown source"));
}

/// The package manager window in a real client: it lists mise's tools,
/// searches, and a confirmed install runs `termos pkg` in a window.
#[test]
fn the_packages_window_installs_in_a_window() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let mise = bin.path().join("mise");
    script(
        &mise,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n\
             --version) echo '2026.10.14 linux-x64 (2026-10-18)';;\n\
             ls) echo '{{\"go\": [{{\"version\": \"1.25.3\", \"installed\": true, \"active\": true}}]}}';;\n\
             registry) printf 'jq   aqua:jqlang/jq\\njqp  aqua:noahgorstein/jqp\\n';;\n\
             *) echo \"$*\" >> '{}';;\n\
             esac\n",
            calls.display()
        ),
    );
    let env = common::Env::with_vars("", vec![("TERMOS_MISE", mise)]);
    let t = common::Termos::start_in(env);
    t.wait_ready();
    let wait = std::time::Duration::from_secs(20);
    assert!(t.env.run(&["msg", "packages.open"]).status.success());
    t.wait_for("the packages window", wait, |s| {
        s.contains("Packages") && s.contains("Dev rootfs (apt)") && s.contains("1.25.3")
    });
    t.send(b"/jq\r");
    t.wait_for("what mise has", wait, |s| s.contains("jqp"));
    t.send(b"\r");
    t.wait_for("the question", wait, |s| {
        s.contains("Install jq with mise? y/n")
    });
    t.send(b"y");
    t.wait_for("its window", wait, |s| s.contains("press Enter to close"));
    let log = std::fs::read_to_string(&calls).expect("calls");
    assert!(log.contains("use --global jq@latest"), "{log}");
}
