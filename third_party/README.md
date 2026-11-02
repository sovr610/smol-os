# Patched dependencies

Copies of crates.io crates with a fix TermOS needs before upstream ships
one, wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Each keeps its own license. Remove a copy (and its `[patch]` line) as soon
as a release of the crate has the fix.

| Crate | Version | License | Change | Why |
| --- | --- | --- | --- | --- |
| `gemm-common` | 0.19.0 | MIT | `#[target_feature(enable = "fp16")]` on the seven FP16 inline-assembly helpers in `src/simd.rs` (marked `TermOS:`) | candle's matrix code (`gemm-f16`) calls them from FP16 kernels that are chosen at run time; where LLVM does not inline them, it rejects their FP16 instructions, so debug builds for aarch64 Linux fail (D-070). |
