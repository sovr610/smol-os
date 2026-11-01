// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-models
//!
//! Local model management and inference.
//!
//! **Status:** not started. Scheduled for M7 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Local models"):
//! - `hub` — Hugging Face search/pull/list/rm with resumable downloads,
//!   checksum verification, cache management and HF token support.
//! - `fit` — memory estimates and size/quantization recommendations for the
//!   detected hardware.
//! - `backend` — llama.cpp (default), ONNX Runtime and/or candle, an optional
//!   Python sidecar (transformers/vLLM), and accelerator-specific paths.
//! - `server` — one local OpenAI-compatible endpoint in front of every backend.

// TODO(M7): HF adapter, hardware-aware recommendations, llama.cpp backend, OpenAI-compatible router.
