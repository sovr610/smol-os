// SPDX-License-Identifier: GPL-3.0-or-later
//! Embedding models in pure Rust with candle (O-7 → D-065, D-067).
//!
//! BERT-family models (sentence-transformers MiniLM, BGE, E5…) from their
//! safetensors weights, `config.json` and `tokenizer.json`, on the CPU:
//! the token embeddings are mean-pooled over the attention mask and
//! normalized, as sentence-transformers and OpenAI's embeddings are.
//! No C or C++ runtime, nothing to install.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// A loaded embedding model.
pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    pub dims: usize,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Embedder({} dimensions)", self.dims)
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Embedder {
    /// Loads the model in a snapshot folder.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let device = Device::Cpu;
        let config: Config = serde_json::from_slice(
            &std::fs::read(dir.join("config.json")).map_err(|e| format!("config.json: {e}"))?,
        )
        .map_err(|e| format!("config.json is not a BERT config: {e}"))?;
        let weights = std::fs::read(dir.join("model.safetensors"))
            .map_err(|e| format!("model.safetensors: {e}"))?;
        let vb =
            VarBuilder::from_buffered_safetensors(weights, DType::F32, &device).map_err(err)?;
        let model = BertModel::load(vb, &config)
            .map_err(|e| format!("the weights do not match the config: {e}"))?;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| format!("tokenizer.json: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id: u32::try_from(config.pad_token_id).unwrap_or(0),
            ..PaddingParams::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: config.max_position_embeddings.min(512),
                ..TruncationParams::default()
            }))
            .map_err(err)?;
        Ok(Self {
            model,
            tokenizer,
            device,
            dims: config.hidden_size,
        })
    }

    /// The embeddings of `texts`, and the tokens read.
    pub fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, usize), String> {
        if texts.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(err)?;
        let tokens: usize = encodings
            .iter()
            .map(|e| e.get_attention_mask().iter().filter(|m| **m == 1).count())
            .sum();
        let rows = |f: &dyn Fn(&tokenizers::Encoding) -> Vec<u32>| -> Result<Tensor, String> {
            let v: Vec<Tensor> = encodings
                .iter()
                .map(|e| Tensor::new(f(e).as_slice(), &self.device))
                .collect::<Result<_, _>>()
                .map_err(err)?;
            Tensor::stack(&v, 0).map_err(err)
        };
        let ids = rows(&|e| e.get_ids().to_vec())?;
        let types = rows(&|e| e.get_type_ids().to_vec())?;
        let mask = rows(&|e| e.get_attention_mask().to_vec())?;
        let out = self.model.forward(&ids, &types, Some(&mask)).map_err(err)?;
        // Mean over the real tokens, then unit length.
        let m = mask
            .to_dtype(DType::F32)
            .map_err(err)?
            .unsqueeze(2)
            .map_err(err)?;
        let summed = out.broadcast_mul(&m).map_err(err)?.sum(1).map_err(err)?;
        let counts = m.sum(1).map_err(err)?;
        let mean = summed.broadcast_div(&counts).map_err(err)?;
        let norm = mean
            .sqr()
            .map_err(err)?
            .sum_keepdim(1)
            .map_err(err)?
            .sqrt()
            .map_err(err)?;
        let unit = mean.broadcast_div(&norm).map_err(err)?;
        Ok((unit.to_vec2::<f32>().map_err(err)?, tokens))
    }
}

/// Writes a tiny random BERT with a word-level vocabulary into `dir`
/// (tests and the demo; no download needed).
pub fn write_tiny_model(dir: &Path) -> Result<(), String> {
    use candle_nn::VarMap;

    std::fs::create_dir_all(dir).map_err(err)?;
    let config = serde_json::json!({
        "architectures": ["BertModel"], "model_type": "bert",
        "vocab_size": 16, "hidden_size": 8, "num_hidden_layers": 1, "num_attention_heads": 2,
        "intermediate_size": 16, "hidden_act": "gelu", "hidden_dropout_prob": 0.0,
        "max_position_embeddings": 32, "type_vocab_size": 2, "initializer_range": 0.02,
        "layer_norm_eps": 1e-12, "pad_token_id": 0
    });
    std::fs::write(dir.join("config.json"), config.to_string()).map_err(err)?;
    let cfg: Config = serde_json::from_value(config).map_err(err)?;
    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu);
    BertModel::load(vb, &cfg).map_err(err)?;
    varmap.save(dir.join("model.safetensors")).map_err(err)?;
    let words = [
        "[PAD]", "[UNK]", "[CLS]", "[SEP]", "hello", "world", "local", "models", "run", "fast",
        "on", "the", "cpu", "a", "b", "c",
    ];
    let vocab: serde_json::Map<String, serde_json::Value> = words
        .iter()
        .enumerate()
        .map(|(i, w)| ((*w).to_owned(), serde_json::json!(i)))
        .collect();
    // The format the Hub's BERT tokenizers use.
    let tokenizer = serde_json::json!({
        "version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
        "normalizer": {"type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true, "strip_accents": null, "lowercase": true},
        "pre_tokenizer": {"type": "BertPreTokenizer"},
        "post_processor": {"type": "BertProcessing", "sep": ["[SEP]", 3], "cls": ["[CLS]", 2]},
        "decoder": null,
        "model": {"type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##", "max_input_chars_per_word": 100, "vocab": vocab}
    });
    std::fs::write(dir.join("tokenizer.json"), tokenizer.to_string()).map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tiny_model_embeds_texts_as_unit_vectors() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_tiny_model(dir.path()).expect("model");
        let e = Embedder::load(dir.path()).expect("load");
        assert_eq!(e.dims, 8);
        let texts = vec![
            "hello world".to_owned(),
            "local models run fast on the cpu".to_owned(),
            "hello world".to_owned(),
        ];
        let (v, tokens) = e.embed(&texts).expect("embed");
        assert_eq!(v.len(), 3);
        assert_eq!(tokens, 4 + 9 + 4, "words plus [CLS] and [SEP]");
        for row in &v {
            assert_eq!(row.len(), 8);
            let norm: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4, "{norm}");
        }
        // The same text gives the same vector, whatever else is in the
        // batch (padding is masked out).
        for (a, b) in v[0].iter().zip(&v[2]) {
            assert!((a - b).abs() < 1e-5);
        }
        let (alone, _) = e.embed(&["hello world".to_owned()]).expect("embed");
        for (a, b) in v[0].iter().zip(&alone[0]) {
            assert!((a - b).abs() < 1e-4, "padding changed the result");
        }
    }
}
