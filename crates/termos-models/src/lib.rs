// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-models
//!
//! Local models (ARCHITECTURE.md §12, D-065 to D-068).
//!
//! - [`hub`]: the Hugging Face Hub's API; [`cache`]: the
//!   `huggingface_hub`-compatible cache with resumable, verified
//!   downloads.
//! - [`gguf`]: GGUF headers; [`fit`]: memory estimates and placement.
//! - [`catalog`]: models by name: pull plans, cached models, their ids.
//! - [`llama`]: llama.cpp's `llama-server`, installed by mise for this
//!   machine's accelerator; `embed` (feature `candle`): embedding models
//!   in-process.
//! - [`router`]: `termos models serve`, one OpenAI-compatible endpoint
//!   that starts backends on demand within a memory budget.
//!
//! Hardware detection is `termos_hw::accel`; the endpoint file agents
//! read is `termos_core::models_endpoint`.

pub mod cache;
pub mod catalog;
#[cfg(feature = "candle")]
pub mod embed;
pub mod fit;
pub mod gguf;
pub mod hub;
pub mod llama;
pub mod router;
