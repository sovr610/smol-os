// SPDX-License-Identifier: GPL-3.0-or-later
//! Changing `config.toml` from a window (the Sound and Displays pages,
//! D-128): the file is edited in place with `toml_edit`, so the user's
//! comments and layout stay; the result is checked as a config before it
//! is written, and written atomically. The caller then asks the desktop
//! to reload (`config.reload`).

use std::path::Path;

use toml_edit::DocumentMut;

/// `config` with `edit` applied (a missing file is empty).
pub fn edit(config: &Path, edit: impl FnOnce(&mut DocumentMut)) -> Result<(), String> {
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", config.display())),
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{}: {e}", config.display()))?;
    edit(&mut doc);
    let new = doc.to_string();
    // Never write a file TermOS would refuse.
    termos_core::config::Config::parse(&new, config).map_err(|e| format!("not saved: {e}"))?;
    if let Some(dir) = config.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot save: {e}"))?;
    }
    let tmp = config.with_extension("toml.tmp");
    std::fs::write(&tmp, new)
        .and_then(|()| std::fs::rename(&tmp, config))
        .map_err(|e| format!("cannot save {}: {e}", config.display()))
}

/// The table `name`, made if missing (as `[name]`, not inline).
pub fn table<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut toml_edit::Table {
    if !doc.contains_table(name) {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        doc.insert(name, toml_edit::Item::Table(t));
    }
    doc[name]
        .as_table_mut()
        .expect("just made sure it is a table")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_refuse_bad_configs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "# mine\nmouse = true\n").expect("write");
        edit(&file, |d| {
            table(d, "sound").insert("device", toml_edit::value("HDMI"));
        })
        .expect("saved");
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.starts_with("# mine\nmouse = true\n"), "{text}");
        assert!(text.contains("[sound]\ndevice = \"HDMI\""), "{text}");
        let err = edit(&file, |d| {
            table(d, "sound").insert("loudness", toml_edit::value(11));
        })
        .expect_err("unknown key");
        assert!(err.starts_with("not saved"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), text);
        // A missing file is made.
        let new = dir.path().join("sub").join("config.toml");
        edit(&new, |d| {
            table(d, "bluetooth").insert("enabled", toml_edit::value(false));
        })
        .expect("saved");
        assert!(new.exists());
    }
}
