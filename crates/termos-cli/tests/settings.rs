// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos settings export` and `import` (M13g, D-126) with the real
//! binary on two temporary homes: what moves, what never does, and the
//! user's config kept where the archive says nothing.

use std::path::Path;
use std::process::{Command, Output};

/// `termos ARGS` with `home`'s folders (and `stdin`).
fn termos(home: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_termos"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write");
    child.wait_with_output().expect("waits")
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn settings_move_and_secrets_stay() {
    let old = tempfile::tempdir().expect("dir");
    let o = old.path();
    put(
        &o.join("config/termos/config.toml"),
        "# old machine\n[theme]\nbase = \"dark\"\n[agents.providers.openai]\nkind = \"openai\"\nkey = \"keyring:openai\"\n",
    );
    put(
        &o.join("config/termos/layouts/dev.toml"),
        "[[pane]]\ncommand = \"nvim .\"\n",
    );
    put(
        &o.join("config/termos/themes/nord.toml"),
        "[theme]\naccent = \"#88c0d0\"\n",
    );
    // What must never leave: the secrets file, histories, snapshots, logs.
    put(
        &o.join("data/termos/secrets.age"),
        "age-encryption.org/v1\n-> X25519 abc\n",
    );
    put(
        &o.join("state/termos/sessions/main.json"),
        "{\"panes\": []}",
    );
    put(
        &o.join("state/termos/log/termos.log"),
        "sk-ant-api03-SECRETSECRETSECRETSECRETSECRET\n",
    );
    put(
        &o.join("data/termos/accounts/accounts.toml"),
        "[accounts.claude.work]\nlabel = \"Work\"\n",
    );
    let archive = o.join("moved.tar.gz");
    let out = termos(o, &["settings", "export", &archive.to_string_lossy()], "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read it back as the importer does, and look for anything secret.
    let files = termos_apps::settings_io::read_archive(&archive).expect("an archive");
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "MANIFEST.txt",
            "accounts.txt",
            "config.toml",
            "layouts/dev.toml",
            "themes/nord.toml"
        ]
    );
    let redactor = termos_ai::redact::Redactor::new();
    for (name, data) in &files {
        let text = String::from_utf8_lossy(data);
        assert_eq!(redactor.text(&text), text, "{name} holds something secret");
        assert!(!text.contains("age-encryption"), "{name}");
    }

    // A config with a key pasted in is refused, and nothing is written.
    put(
        &o.join("config/termos/config.toml"),
        "[agents.providers.openai]\nkind = \"openai\"\nkey = \"sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"\n",
    );
    let refused = o.join("refused.tar.gz");
    let out = termos(o, &["settings", "export", &refused.to_string_lossy()], "");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("config.toml:3"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!refused.exists());

    // The new machine: its own comment and icons stay, the archive's
    // theme wins, its layout differs and is kept on "no".
    let new = tempfile::tempdir().expect("dir");
    let n = new.path();
    put(
        &n.join("config/termos/config.toml"),
        "# new machine\n[ui]\nicons = \"ascii\"\n[theme]\nbase = \"light\"\n",
    );
    put(
        &n.join("config/termos/layouts/dev.toml"),
        "[[pane]]\ncommand = \"vim\"\n",
    );
    let out = termos(
        n,
        &["settings", "import", &archive.to_string_lossy()],
        "y\nn\n",
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let config = std::fs::read_to_string(n.join("config/termos/config.toml")).expect("read");
    assert!(config.starts_with("# new machine"), "{config}");
    assert!(config.contains("icons = \"ascii\""), "{config}");
    assert!(config.contains("base = \"dark\""), "{config}");
    assert!(config.contains("keyring:openai"), "{config}");
    assert_eq!(
        std::fs::read_to_string(n.join("config/termos/layouts/dev.toml")).expect("read"),
        "[[pane]]\ncommand = \"vim\"\n"
    );
    assert!(n.join("config/termos/themes/nord.toml").is_file());
    assert!(said.contains("termos account add claude work"), "{said}");
    assert!(!n.join("data/termos/secrets.age").exists());
}
