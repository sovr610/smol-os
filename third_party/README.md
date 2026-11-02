# Patched dependencies

Copies of crates.io crates with a fix TermOS needs before upstream ships
one, wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Each keeps its own license. Remove a copy (and its `[patch]` line) as soon
as a release of the crate has the fix.

| Crate | Version | License | Change | Why |
| --- | --- | --- | --- | --- |
| `gemm-common` | 0.19.0 | MIT | `#[target_feature(enable = "fp16")]` on the seven FP16 inline-assembly helpers in `src/simd.rs` (marked `TermOS:`) | candle's matrix code (`gemm-f16`) calls them from FP16 kernels that are chosen at run time; where LLVM does not inline them, it rejects their FP16 instructions, so debug builds for aarch64 Linux fail (D-070). |
| `gemm-common` | 0.19.0 | MIT | `#[allow(unreachable_code)]` on `cache_info` in `src/cache.rs` (marked `TermOS:`) | On x86_64 macOS its Apple branch returns before the x86 one. As a path dependency its warnings are not capped, so release builds for that target, which deny warnings, failed. Goes with the copy. |
