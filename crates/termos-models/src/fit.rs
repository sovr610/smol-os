// SPDX-License-Identifier: GPL-3.0-or-later
//! Will it fit? The memory a model needs, where it can go (all on the GPU,
//! split between GPU and CPU, or CPU only), and which of a repository's
//! files to choose (ARCHITECTURE.md §12, D-066).
//!
//! An estimate, shown before anything is downloaded:
//! - weights: the files' sizes (exact);
//! - KV cache: layers × context × KV heads × (key + value dims) × 2 bytes
//!   (f16), from the GGUF header or `config.json`;
//! - overhead: 256 MiB plus 5% of the weights (compute buffers, the
//!   runtime);
//! - headroom: 10% of the memory it goes into stays free.

use std::sync::LazyLock;

use regex::Regex;
use termos_hw::accel::Accelerators;

/// What the KV cache depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub layers: u64,
    pub kv_heads: u64,
    pub key_dim: u64,
    pub value_dim: u64,
    /// The context the model was trained for, if known.
    pub context: Option<u64>,
}

/// Bytes a model needs at a context length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub weights: u64,
    pub kv_cache: u64,
    pub overhead: u64,
    pub context: u64,
    pub layers: u64,
}

impl Estimate {
    pub fn new(weights: u64, shape: Option<&Shape>, context: u64) -> Self {
        let (kv_cache, context, layers) = match shape {
            Some(s) => {
                let ctx = s.context.map_or(context, |c| context.min(c));
                // Saturating: the numbers come from downloaded headers.
                let kv = s
                    .layers
                    .saturating_mul(ctx)
                    .saturating_mul(s.kv_heads)
                    .saturating_mul(s.key_dim.saturating_add(s.value_dim))
                    .saturating_mul(2);
                (kv, ctx, s.layers)
            }
            // Unknown shape: a cautious guess of 10% of the weights.
            None => (weights / 10, context, 0),
        };
        Self {
            weights,
            kv_cache,
            overhead: (256u64 << 20).saturating_add(weights / 20),
            context,
            layers,
        }
    }

    pub fn total(&self) -> u64 {
        self.weights
            .saturating_add(self.kv_cache)
            .saturating_add(self.overhead)
    }
}

/// Where a model can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Every layer on the GPU (or in unified memory).
    Gpu,
    /// `gpu_layers` of `layers` on the GPU, the rest on the CPU.
    Partial {
        gpu_layers: u64,
        layers: u64,
    },
    Cpu,
    /// Not enough memory anywhere.
    TooBig,
}

impl Placement {
    /// llama.cpp's `--n-gpu-layers` for it.
    pub fn gpu_layers(self) -> u64 {
        match self {
            Self::Gpu => 999,
            Self::Partial { gpu_layers, .. } => gpu_layers,
            Self::Cpu | Self::TooBig => 0,
        }
    }

    /// Better placements sort first.
    fn rank(self) -> u8 {
        match self {
            Self::Gpu => 0,
            Self::Partial { .. } => 1,
            Self::Cpu => 2,
            Self::TooBig => 3,
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Gpu => "fits on the GPU".into(),
            Self::Partial { gpu_layers, layers } => {
                format!("{gpu_layers} of {layers} layers on the GPU, the rest on the CPU")
            }
            Self::Cpu => "runs on the CPU".into(),
            Self::TooBig => "too large for this machine".into(),
        }
    }
}

/// With 10% of `mem` kept free, does `need` fit?
fn fits(need: u64, mem: u64) -> bool {
    need.saturating_add(need / 10) <= mem
}

/// Where `est` can run on `hw` (`gpu`: a GPU build of llama.cpp can be
/// used).
pub fn place(est: &Estimate, hw: &Accelerators, gpu: bool) -> Placement {
    let ram = hw.memory_available;
    if gpu && (hw.metal || (hw.unified_memory && hw.pi_model.is_none())) {
        // Unified memory: the GPU uses system memory.
        return if fits(est.total(), ram) {
            Placement::Gpu
        } else {
            Placement::TooBig
        };
    }
    if gpu && let Some(vram) = hw.best_gpu_free() {
        if fits(est.total(), vram) {
            return Placement::Gpu;
        }
        if let Some(per_layer) = est
            .weights
            .saturating_add(est.kv_cache)
            .checked_div(est.layers)
        {
            let usable = (vram - vram / 10).saturating_sub(est.overhead);
            let gpu_layers = usable
                .checked_div(per_layer)
                .map_or(0, |n| n.min(est.layers));
            let cpu_part = (est.layers - gpu_layers)
                .saturating_mul(per_layer)
                .saturating_add(est.overhead);
            if gpu_layers > 0 && fits(cpu_part, ram) {
                return Placement::Partial {
                    gpu_layers,
                    layers: est.layers,
                };
            }
        }
    }
    if fits(est.total(), ram) {
        Placement::Cpu
    } else {
        Placement::TooBig
    }
}

static QUANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[-_.])((?:I?Q[1-8](?:_[0-9A-Z]+)*)|BF16|F16|F32|FP16|FP32|MXFP4|TQ[12]_0)(?:[-_.]|$)")
        .expect("a valid built-in pattern")
});

/// The quantization in a file name (`Q4_K_M`, `IQ3_XXS`, `F16`…).
pub fn quant_label(file: &str) -> Option<String> {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.strip_suffix(".gguf").unwrap_or(stem);
    QUANT
        .captures_iter(stem)
        .last()
        .map(|c| c[1].to_ascii_uppercase())
}

/// One choice among a repository's files.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub file: String,
    pub quant: Option<String>,
    pub estimate: Estimate,
    pub placement: Placement,
}

/// Ranks a repository's files: the best placement first, and within it
/// the largest file (more bits keep more quality).
pub fn rank(
    files: &[(String, u64)],
    shape: Option<&Shape>,
    context: u64,
    hw: &Accelerators,
    gpu: bool,
) -> Vec<Choice> {
    let mut choices: Vec<Choice> = files
        .iter()
        .map(|(file, size)| {
            let estimate = Estimate::new(*size, shape, context);
            Choice {
                file: file.clone(),
                quant: quant_label(file),
                placement: place(&estimate, hw, gpu),
                estimate,
            }
        })
        .collect();
    choices.sort_by(|a, b| {
        a.placement
            .rank()
            .cmp(&b.placement.rank())
            .then(b.estimate.weights.cmp(&a.estimate.weights))
    });
    choices
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_hw::GpuVendor;
    use termos_hw::accel::GpuMemory;

    const GIB: u64 = 1 << 30;

    fn llama3_8b() -> Shape {
        Shape {
            layers: 32,
            kv_heads: 8,
            key_dim: 128,
            value_dim: 128,
            context: Some(8192),
        }
    }

    fn machine(ram: u64, vram: Option<u64>) -> Accelerators {
        Accelerators {
            memory_total: ram,
            memory_available: ram,
            gpus: vram
                .map(|v| GpuMemory {
                    name: "gpu".into(),
                    vendor: GpuVendor::Nvidia,
                    total: Some(v),
                    free: Some(v),
                })
                .into_iter()
                .collect(),
            ..Accelerators::default()
        }
    }

    #[test]
    fn kv_cache_matches_the_published_numbers() {
        // Llama 3 8B at 8192 tokens, f16: 1 GiB of KV cache.
        let e = Estimate::new(4_920_000_000, Some(&llama3_8b()), 8192);
        assert_eq!(e.kv_cache, GIB);
        // Asking for more context than the model has is capped.
        assert_eq!(Estimate::new(1, Some(&llama3_8b()), 100_000).context, 8192);
        assert_eq!(Estimate::new(1, Some(&llama3_8b()), 4096).kv_cache, GIB / 2);
    }

    #[test]
    fn placements() {
        let q4 = Estimate::new(4_920_000_000, Some(&llama3_8b()), 8192); // ≈ 6.0 GiB in all
        assert_eq!(
            place(&q4, &machine(32 * GIB, Some(24 * GIB)), true),
            Placement::Gpu
        );
        assert_eq!(
            place(&q4, &machine(32 * GIB, Some(24 * GIB)), false),
            Placement::Cpu,
            "a CPU build"
        );
        match place(&q4, &machine(32 * GIB, Some(4 * GIB)), true) {
            Placement::Partial { gpu_layers, layers } => {
                assert!(gpu_layers > 10 && gpu_layers < 32 && layers == 32)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(place(&q4, &machine(16 * GIB, None), true), Placement::Cpu);
        assert_eq!(place(&q4, &machine(4 * GIB, None), true), Placement::TooBig);
        let mut mac = machine(16 * GIB, None);
        mac.metal = true;
        mac.unified_memory = true;
        assert_eq!(place(&q4, &mac, true), Placement::Gpu);
    }

    #[test]
    fn the_largest_file_that_fits_comes_first() {
        let files = vec![
            ("m-Q2_K.gguf".to_owned(), 3_180_000_000),
            ("m-Q4_K_M.gguf".to_owned(), 4_920_000_000),
            ("m-Q8_0.gguf".to_owned(), 8_540_000_000),
            ("m-F16.gguf".to_owned(), 16_070_000_000),
        ];
        let r = rank(
            &files,
            Some(&llama3_8b()),
            8192,
            &machine(64 * GIB, Some(8 * GIB)),
            true,
        );
        assert_eq!(r[0].quant.as_deref(), Some("Q4_K_M"));
        assert_eq!(r[0].placement, Placement::Gpu);
        assert_eq!(
            (r[1].quant.as_deref(), r[1].placement),
            (Some("Q2_K"), Placement::Gpu)
        );
        assert!(
            matches!(r[2].placement, Placement::Partial { .. }),
            "{:?}",
            r[2]
        );
        let cpu_only = rank(
            &files,
            Some(&llama3_8b()),
            8192,
            &machine(12 * GIB, None),
            true,
        );
        assert_eq!(cpu_only[0].quant.as_deref(), Some("Q8_0"));
        assert_eq!(
            cpu_only.last().map(|c| c.placement),
            Some(Placement::TooBig)
        );
    }

    #[test]
    fn quant_labels() {
        for (f, q) in [
            ("Meta-Llama-3-8B-Instruct-Q4_K_M.gguf", Some("Q4_K_M")),
            ("qwen2.5-0.5b-instruct-q8_0.gguf", Some("Q8_0")),
            ("gemma-2-2b-it-IQ3_XXS.gguf", Some("IQ3_XXS")),
            ("model-bf16-00001-of-00002.gguf", Some("BF16")),
            ("tinyllamas/stories260K.gguf", None),
            ("ggml-model-f16.gguf", Some("F16")),
        ] {
            assert_eq!(quant_label(f).as_deref(), q, "{f}");
        }
    }

    #[test]
    fn absurd_headers_do_not_overflow() {
        let huge = Shape {
            layers: u64::MAX,
            kv_heads: u64::MAX,
            key_dim: u64::MAX,
            value_dim: 1,
            context: None,
        };
        let e = Estimate::new(u64::MAX, Some(&huge), u64::MAX);
        assert_eq!(e.total(), u64::MAX);
        let hw = Accelerators {
            memory_available: 8 * GIB,
            gpus: vec![GpuMemory {
                name: "g".into(),
                vendor: GpuVendor::Nvidia,
                total: Some(8 * GIB),
                free: Some(8 * GIB),
            }],
            ..Accelerators::default()
        };
        assert_eq!(place(&e, &hw, true), Placement::TooBig);
    }
}
