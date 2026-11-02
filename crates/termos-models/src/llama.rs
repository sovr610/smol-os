// SPDX-License-Identifier: GPL-3.0-or-later
//! llama.cpp, run as `llama-server` subprocesses (O-6 → D-065): one per
//! loaded model, on a local port, behind the router.
//!
//! The binary is the user's own (`[models] llama-server`, a CUDA or ROCm
//! build, say) or llama.cpp's official release for this machine, which
//! mise installs (its `github:` backend, as for other tools, D-053):
//! Metal on Apple Silicon; Vulkan where a GPU and the Vulkan loader are
//! present; CPU otherwise. The official releases have no Linux CUDA or
//! ROCm builds, and the Windows CUDA build needs CUDA's runtime libraries
//! besides the driver, so CUDA and ROCm are chosen only by hand; Vulkan
//! runs on NVIDIA and AMD drivers alone.

use std::path::{Path, PathBuf};

use termos_core::config::models::LlamaBuild;
use termos_hw::accel::Accelerators;

/// The llama.cpp repository mise installs from.
pub const REPO: &str = "ggml-org/llama.cpp";

/// The build to use: the config's choice, else one from the hardware.
pub fn choose(config: LlamaBuild, hw: &Accelerators) -> LlamaBuild {
    if config != LlamaBuild::Auto {
        return config;
    }
    if hw.metal {
        LlamaBuild::Metal
    } else if hw.vulkan && !hw.gpus.is_empty() && hw.pi_model.is_none() {
        LlamaBuild::Vulkan
    } else {
        LlamaBuild::Cpu
    }
}

/// Whether a build runs layers on a GPU.
pub fn uses_gpu(build: LlamaBuild) -> bool {
    !matches!(build, LlamaBuild::Cpu | LlamaBuild::Auto)
}

/// The release asset for a build on this platform (a glob for mise's
/// `asset_pattern`), or why there is none.
pub fn asset_pattern(build: LlamaBuild, os: &str, arch: &str) -> Result<&'static str, String> {
    let none = |what: &str| {
        Err(format!(
            "llama.cpp publishes no {what} build for {os} {arch}: set [models] llama-cpp to another build, \
             or [models] llama-server to a llama-server you built"
        ))
    };
    Ok(match (os, arch, build) {
        ("linux", "x86_64", LlamaBuild::Cpu | LlamaBuild::Auto) => "llama-*-bin-ubuntu-x64.*",
        ("linux", "x86_64", LlamaBuild::Vulkan) => "llama-*-bin-ubuntu-vulkan-x64.*",
        ("linux", "aarch64", LlamaBuild::Cpu | LlamaBuild::Auto) => "llama-*-bin-ubuntu-arm64.*",
        ("linux", "aarch64", LlamaBuild::Vulkan) => "llama-*-bin-ubuntu-vulkan-arm64.*",
        ("macos", "aarch64", LlamaBuild::Metal | LlamaBuild::Cpu | LlamaBuild::Auto) => {
            "llama-*-bin-macos-arm64.*"
        }
        ("macos", "x86_64", LlamaBuild::Cpu | LlamaBuild::Auto) => "llama-*-bin-macos-x64.*",
        ("windows", "x86_64", LlamaBuild::Cpu | LlamaBuild::Auto) => "llama-*-bin-win-cpu-x64.*",
        ("windows", "x86_64", LlamaBuild::Vulkan) => "llama-*-bin-win-vulkan-x64.*",
        ("windows", "x86_64", LlamaBuild::Cuda) => "llama-*-bin-win-cuda-12*-x64.*",
        ("windows", "x86_64", LlamaBuild::Rocm) => "llama-*-bin-win-hip-radeon-x64.*",
        ("windows", "aarch64", LlamaBuild::Cpu | LlamaBuild::Auto) => "llama-*-bin-win-cpu-arm64.*",
        (_, _, b) => return none(b.name()),
    })
}

/// The mise tool for a build: `github:ggml-org/llama.cpp[asset_pattern=…]@<version>`.
pub fn mise_tool(build: LlamaBuild, os: &str, arch: &str, version: &str) -> Result<String, String> {
    Ok(format!(
        "github:{REPO}[asset_pattern={}]@{version}",
        asset_pattern(build, os, arch)?
    ))
}

/// Where `llama-server` comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A binary given in the config.
    Path(PathBuf),
    /// Installed by mise.
    Mise { mise: PathBuf, tool: String },
}

/// Finds `llama-server` below an install folder.
pub fn find_server(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if e.file_name() == name {
                return Some(p);
            }
        }
    }
    None
}

impl Source {
    /// The `llama-server` binary, installing it first if it comes from
    /// mise. Blocking (mise downloads); call it off the event loop.
    pub fn binary(&self) -> Result<PathBuf, String> {
        match self {
            Self::Path(p) => {
                if p.is_file() {
                    Ok(p.clone())
                } else {
                    Err(format!(
                        "[models] llama-server: {} does not exist",
                        p.display()
                    ))
                }
            }
            Self::Mise { mise, tool } => {
                let run = |args: &[&str]| {
                    std::process::Command::new(mise)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| format!("cannot run mise: {e}"))
                };
                let out = run(&["install", tool])?;
                if !out.status.success() {
                    return Err(format!(
                        "mise could not install llama.cpp ({tool}): {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                let out = run(&["where", tool])?;
                let dir = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if !out.status.success() || dir.is_empty() {
                    return Err(format!("mise does not know where llama.cpp went ({tool})"));
                }
                find_server(Path::new(&dir))
                    .ok_or_else(|| format!("no llama-server in llama.cpp's release ({dir})"))
            }
        }
    }
}

/// The environment variable llama-server reads its API key from (it is
/// kept off the command line, which every local user can read).
pub const KEY_VAR: &str = "LLAMA_API_KEY";

/// How to start `llama-server` for one model (its key goes in
/// [`KEY_VAR`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub model: PathBuf,
    pub alias: String,
    pub port: u16,
    pub context: u64,
    pub gpu_layers: u64,
    /// Use the model's own chat template (Jinja).
    pub jinja: bool,
    /// An embedding model.
    pub embeddings: bool,
}

impl Launch {
    pub fn args(&self) -> Vec<String> {
        let mut a = vec![
            "--model".into(),
            self.model.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
            "--ctx-size".into(),
            self.context.to_string(),
            "--n-gpu-layers".into(),
            self.gpu_layers.to_string(),
            "--alias".into(),
            self.alias.clone(),
        ];
        if self.jinja {
            a.push("--jinja".into());
        }
        if self.embeddings {
            a.push("--embeddings".into());
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_hw::GpuVendor;
    use termos_hw::accel::GpuMemory;

    fn hw(os: &str) -> Accelerators {
        Accelerators {
            os: os.into(),
            arch: "x86_64".into(),
            ..Accelerators::default()
        }
    }

    #[test]
    fn builds_follow_the_hardware() {
        let mut a = hw("linux");
        assert_eq!(choose(LlamaBuild::Auto, &a), LlamaBuild::Cpu);
        a.gpus.push(GpuMemory {
            name: "g".into(),
            vendor: GpuVendor::Amd,
            total: Some(1),
            free: Some(1),
        });
        assert_eq!(
            choose(LlamaBuild::Auto, &a),
            LlamaBuild::Cpu,
            "a GPU without Vulkan"
        );
        a.vulkan = true;
        assert_eq!(choose(LlamaBuild::Auto, &a), LlamaBuild::Vulkan);
        assert_eq!(
            choose(LlamaBuild::Cpu, &a),
            LlamaBuild::Cpu,
            "the config wins"
        );
        let mut w = hw("windows");
        w.cuda = true;
        assert_eq!(
            choose(LlamaBuild::Auto, &w),
            LlamaBuild::Cpu,
            "CUDA's runtime may be missing"
        );
        w.vulkan = true;
        w.gpus = a.gpus.clone();
        assert_eq!(
            choose(LlamaBuild::Auto, &w),
            LlamaBuild::Vulkan,
            "the driver brings Vulkan"
        );
        let mut m = hw("macos");
        m.metal = true;
        assert_eq!(choose(LlamaBuild::Auto, &m), LlamaBuild::Metal);
        let mut pi = hw("linux");
        pi.vulkan = true;
        pi.gpus = a.gpus.clone();
        pi.pi_model = Some("Raspberry Pi 5".into());
        assert_eq!(
            choose(LlamaBuild::Auto, &pi),
            LlamaBuild::Cpu,
            "a Pi runs llama.cpp on its CPU"
        );
    }

    #[test]
    fn assets_and_tools() {
        assert_eq!(
            mise_tool(LlamaBuild::Cpu, "linux", "x86_64", "latest").expect("tool"),
            "github:ggml-org/llama.cpp[asset_pattern=llama-*-bin-ubuntu-x64.*]@latest"
        );
        assert!(
            asset_pattern(LlamaBuild::Cuda, "linux", "x86_64")
                .expect_err("none")
                .contains("llama-server")
        );
        assert!(asset_pattern(LlamaBuild::Metal, "linux", "x86_64").is_err());
        let l = Launch {
            model: PathBuf::from("/m/x.gguf"),
            alias: "org/x".into(),
            port: 4242,
            context: 4096,
            gpu_layers: 999,
            jinja: true,
            embeddings: false,
        };
        assert_eq!(
            l.args().join(" "),
            "--model /m/x.gguf --host 127.0.0.1 --port 4242 --ctx-size 4096 --n-gpu-layers 999 --alias org/x --jinja"
        );
    }

    #[test]
    fn the_server_is_found_in_any_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        std::fs::create_dir_all(dir.path().join("build/bin")).expect("mkdir");
        std::fs::write(dir.path().join("build/bin").join(name), b"").expect("write");
        assert_eq!(
            find_server(dir.path()),
            Some(dir.path().join("build/bin").join(name))
        );
        assert!(Source::Path(dir.path().join("nope")).binary().is_err());
    }
}
