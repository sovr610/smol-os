// SPDX-License-Identifier: GPL-3.0-or-later
//! `[models]`: local models (M7, ARCHITECTURE.md §12, D-065).
//!
//! Only the shape lives here; `termos-models` gives it meaning.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer};

use super::agents::SecretRef;

/// `[models]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ModelsConfig {
    /// The Hugging Face cache (default: `$HF_HUB_CACHE`, `$HF_HOME/hub`,
    /// `~/.cache/huggingface/hub`), shared with other tools.
    pub cache: Option<PathBuf>,
    /// `termos model gc` removes the least recently used models until the
    /// cache is this small (none by default).
    pub cache_limit: Option<ByteSize>,
    /// A Hugging Face token, as `keyring:<name>`, for gated and private
    /// models (`HF_TOKEN` is used too).
    pub token: Option<SecretRef>,
    /// Context length llama.cpp gets (tokens).
    pub context: u32,
    /// Which llama.cpp build: chosen from the hardware, or forced.
    pub llama_cpp: LlamaBuild,
    /// Your own `llama-server` (a CUDA or ROCm build, say); none: TermOS
    /// installs the official build with mise.
    pub llama_server: Option<PathBuf>,
    /// Memory loaded models may take; `None`: 90% of RAM plus VRAM.
    pub memory_budget: Option<ByteSize>,
    /// The router's port; 0 picks a free one.
    pub port: u16,
    /// OpenAI-compatible servers the router starts for some models (the
    /// Python sidecar, D-065).
    pub servers: BTreeMap<String, ServerConfig>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            cache: None,
            cache_limit: None,
            token: None,
            context: 4096,
            llama_cpp: LlamaBuild::Auto,
            llama_server: None,
            memory_budget: None,
            port: 0,
            servers: BTreeMap::new(),
        }
    }
}

/// A llama.cpp build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlamaBuild {
    /// From the hardware: Metal on Apple Silicon, Vulkan with a GPU, else
    /// CPU.
    #[default]
    Auto,
    Cpu,
    Vulkan,
    Cuda,
    Rocm,
    Metal,
}

impl LlamaBuild {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Vulkan => "vulkan",
            Self::Cuda => "cuda",
            Self::Rocm => "rocm",
            Self::Metal => "metal",
        }
    }
}

impl fmt::Display for LlamaBuild {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `[models.servers.<name>]`: a program speaking the OpenAI API on a port.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ServerConfig {
    /// The program and its arguments; `{model}` (the snapshot folder),
    /// `{repo}`, `{port}` and `{token}` are filled in.
    pub command: Vec<String>,
    /// The repositories it serves.
    pub models: Vec<String>,
    /// Environment for it.
    pub env: BTreeMap<String, String>,
    /// Seconds it may take to start (large models load slowly).
    pub start_timeout: Option<u64>,
}

/// A size written as `"16G"`, `"512M"` or a number of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let split = t
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let n: f64 = num
            .parse()
            .map_err(|_| format!("`{s}` is not a size (like 16G, 512M or 1048576)"))?;
        let mult: f64 = match unit
            .trim()
            .to_ascii_uppercase()
            .trim_end_matches("IB")
            .trim_end_matches('B')
        {
            "" => 1.0,
            "K" => 1024.0,
            "M" => 1024.0 * 1024.0,
            "G" => 1024.0 * 1024.0 * 1024.0,
            "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            _ => return Err(format!("`{s}`: unknown unit (use K, M, G or T)")),
        };
        Ok(Self((n * mult) as u64))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&human_bytes(self.0))
    }
}

/// `1.5 GiB`, `300 MiB`, `12 KiB`, `5 B`.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(Self(n)),
            Raw::S(s) => Self::parse(&s).map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(ByteSize::parse("16G"), Ok(ByteSize(16 << 30)));
        assert_eq!(ByteSize::parse("512 MiB"), Ok(ByteSize(512 << 20)));
        assert_eq!(ByteSize::parse("1.5g"), Ok(ByteSize(3 << 29)));
        assert_eq!(ByteSize::parse("4096"), Ok(ByteSize(4096)));
        assert!(ByteSize::parse("lots").is_err());
        assert!(ByteSize::parse("3X").is_err());
        assert_eq!(human_bytes(5), "5 B");
        assert_eq!(human_bytes(3 << 29), "1.5 GiB");
        assert_eq!(human_bytes(300 << 20), "300 MiB");
    }
}
