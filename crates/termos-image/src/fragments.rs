// SPDX-License-Identifier: GPL-3.0-or-later
//! Buildroot configuration fragments, and checking that each setting
//! survived `olddefconfig`. Kconfig drops an option it does not know, or
//! whose dependencies are not met, without a word; here that is an error
//! naming the option.

use std::collections::BTreeMap;

/// One setting: `Some(value)` for `BR2_X=value`, `None` for
/// `# BR2_X is not set`.
pub type Settings = BTreeMap<String, Option<String>>;

/// Reads `BR2_*` settings from a fragment or `.config`.
pub fn parse(text: &str) -> Settings {
    let mut out = Settings::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("# ")
            && let Some(name) = rest.strip_suffix(" is not set")
            && name.starts_with("BR2_")
        {
            out.insert(name.to_owned(), None);
        } else if let Some((name, value)) = line.split_once('=')
            && name.starts_with("BR2_")
            && !line.starts_with('#')
        {
            out.insert(name.to_owned(), Some(value.to_owned()));
        }
    }
    out
}

/// Settings `wanted` asks for that `config` does not have.
pub fn missing(wanted: &Settings, config: &Settings) -> Vec<String> {
    let mut out = Vec::new();
    for (name, want) in wanted {
        let have = config.get(name).cloned().flatten();
        match (want, have) {
            (Some(w), Some(h)) if *w == h => {}
            (None, None) => {}
            (None, Some(h)) if h == "n" => {}
            (Some(w), h) => out.push(format!(
                "{name}={w} (it is {}): unknown to this Buildroot, or its dependencies are not met",
                h.as_deref().unwrap_or("not set")
            )),
            (None, Some(h)) => out.push(format!("{name} should not be set, but is {h}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_and_compare() {
        let frag = parse(
            "# a comment\nBR2_INIT_BUSYBOX=y\nBR2_TARGET_GENERIC_HOSTNAME=\"termos\"\n\
             # BR2_TARGET_GENERIC_GETTY is not set\nBR2_PACKAGE_NOPE=y\n# BR2_PACKAGE_GONE is not set\n",
        );
        assert_eq!(frag.len(), 5);
        let config = parse(
            "BR2_INIT_BUSYBOX=y\nBR2_TARGET_GENERIC_HOSTNAME=\"termos\"\n\
             # BR2_TARGET_GENERIC_GETTY is not set\nBR2_PACKAGE_GONE=y\n",
        );
        let m = missing(&frag, &config);
        assert_eq!(m.len(), 2, "{m:?}");
        assert!(
            m[0].starts_with("BR2_PACKAGE_GONE should not be set"),
            "{m:?}"
        );
        assert!(
            m[1].starts_with("BR2_PACKAGE_NOPE=y (it is not set)"),
            "{m:?}"
        );
    }
}
