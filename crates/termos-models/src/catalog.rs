// SPDX-License-Identifier: GPL-3.0-or-later
//! Models by name: what `termos model pull` fetches, what is in the cache,
//! and the ids the router serves them under (D-066).
//!
//! A model is named `REPO` or `REPO:SELECTOR`, where the selector is a
//! quantization (`q4_k_m`) or a file path (`tinyllamas/stories260K.gguf`).
//! - GGUF repositories: one file (with its parts, when split) is chosen,
//!   by the selector or by what fits this machine best; llama.cpp runs it.
//! - Other repositories with `config.json` and safetensors weights: the
//!   files a BERT-family embedding model needs; candle runs it.

use std::path::{Path, PathBuf};

use serde_json::Value;
use termos_hw::accel::Accelerators;

use crate::cache::{Cache, CachedRepo};
use crate::fit::{self, Choice, Estimate, Shape};
use crate::gguf;
use crate::hub::{Hub, HubError, RepoFile, valid_repo};

/// `REPO[:SELECTOR]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub repo: String,
    pub selector: Option<String>,
    pub revision: String,
}

impl ModelRef {
    pub fn parse(s: &str, revision: Option<&str>) -> Result<Self, String> {
        let (repo, selector) = match s.split_once(':') {
            Some((r, sel)) if !sel.is_empty() => (r, Some(sel.to_owned())),
            Some((r, _)) => (r, None),
            None => (s, None),
        };
        if !valid_repo(repo) {
            return Err(format!(
                "`{repo}` is not a Hugging Face repository (like org/name)"
            ));
        }
        Ok(Self {
            repo: repo.to_owned(),
            selector,
            revision: revision.unwrap_or("main").to_owned(),
        })
    }
}

/// What a model is, for the backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A GGUF file (the first part, when split) for llama.cpp.
    Gguf { file: String },
    /// A BERT-family embedding model (safetensors) for candle.
    Embedding,
    /// Served by a configured server (`[models.servers]`).
    Server { name: String },
}

/// A downloaded model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    /// What clients call it (`/v1/models`).
    pub id: String,
    pub repo: String,
    pub commit: String,
    /// The snapshot folder.
    pub dir: PathBuf,
    pub kind: Kind,
    /// Bytes of weights.
    pub size: u64,
}

impl LocalModel {
    /// The GGUF file's path, for llama.cpp.
    pub fn gguf_path(&self) -> Option<PathBuf> {
        match &self.kind {
            Kind::Gguf { file } => Some(self.dir.join(file)),
            _ => None,
        }
    }
}

/// A GGUF file that is not a model on its own: a later part of a split
/// file, or a vision projector.
fn secondary_gguf(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.contains("mmproj") {
        return true;
    }
    split_part(&name).is_some_and(|(n, _)| n != 1)
}

/// `name-00002-of-00005.gguf` → (2, 5).
fn split_part(name: &str) -> Option<(u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let (head, total) = stem.rsplit_once("-of-")?;
    let (_, part) = head.rsplit_once('-')?;
    if part.len() == 5 && total.len() == 5 {
        Some((part.parse().ok()?, total.parse().ok()?))
    } else {
        None
    }
}

/// The parts of a split GGUF file (or just the file).
pub fn parts_of<'a>(first: &str, files: &'a [RepoFile]) -> Vec<&'a RepoFile> {
    let lower = first.to_ascii_lowercase();
    match split_part(&lower) {
        Some((_, total)) => {
            let prefix = &first[..first.len() - "-00001-of-00000.gguf".len()];
            files
                .iter()
                .filter(|f| {
                    f.path.starts_with(prefix)
                        && split_part(&f.path.to_ascii_lowercase()).is_some_and(|(_, t)| t == total)
                })
                .collect()
        }
        None => files.iter().filter(|f| f.path == first).collect(),
    }
}

/// Files an embedding model needs.
const EMBEDDING_FILES: &[&str] = &[
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "modules.json",
    "1_Pooling/config.json",
];

/// What a pull will fetch.
#[derive(Debug, Clone)]
pub struct Plan {
    pub repo: String,
    pub revision: String,
    pub commit: String,
    pub files: Vec<RepoFile>,
    pub kind: Kind,
    /// The ranked GGUF choices (empty for other kinds).
    pub choices: Vec<Choice>,
    /// The estimate for what will be fetched.
    pub estimate: Option<Estimate>,
    pub placement: Option<fit::Placement>,
}

impl Plan {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Why a model cannot be planned.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error("{0}")]
    Choice(String),
}

/// Reads a GGUF header from the Hub, fetching more until it is complete.
pub async fn remote_header(
    hub: &Hub,
    repo: &str,
    commit: &str,
    path: &str,
) -> Option<gguf::Header> {
    let mut want: u64 = 1 << 20;
    loop {
        let bytes = hub.head_bytes(repo, commit, path, want).await.ok()?;
        match gguf::parse(&bytes) {
            Ok(h) => return Some(h),
            Err(gguf::GgufError::Incomplete(n))
                if n <= gguf::MAX_HEADER && (bytes.len() as u64) == want =>
            {
                want = n.max(want * 2).min(gguf::MAX_HEADER);
            }
            Err(_) => return None,
        }
    }
}

/// Reads a local GGUF file's header.
pub fn local_header(path: &Path) -> Option<gguf::Header> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    let mut want: u64 = 1 << 20;
    loop {
        buf.resize(usize::try_from(want).ok()?, 0);
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => return None,
            }
        }
        match gguf::parse(&buf[..n]) {
            Ok(h) => return Some(h),
            Err(gguf::GgufError::Incomplete(k)) if (n as u64) == want && k <= gguf::MAX_HEADER => {
                want = k.max(want * 2).min(gguf::MAX_HEADER);
                f = std::fs::File::open(path).ok()?;
            }
            Err(_) => return None,
        }
    }
}

/// The shape in a `config.json`.
pub fn config_shape(v: &Value) -> Option<Shape> {
    let get = |k: &str| v.get(k).and_then(Value::as_u64);
    let layers = get("num_hidden_layers")?;
    let heads = get("num_attention_heads")?;
    let hidden = get("hidden_size")?;
    let head_dim = get("head_dim").unwrap_or_else(|| hidden.checked_div(heads).unwrap_or(0));
    Some(Shape {
        layers,
        kv_heads: get("num_key_value_heads").unwrap_or(heads),
        key_dim: head_dim,
        value_dim: head_dim,
        context: get("max_position_embeddings"),
    })
}

/// Whether a `config.json` is a BERT-family model candle can embed with.
pub fn is_bert(v: &Value) -> bool {
    let arch_ok = v
        .get("architectures")
        .and_then(Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .any(|s| s.starts_with("Bert"))
        });
    arch_ok || v.get("model_type").and_then(Value::as_str) == Some("bert")
}

/// Plans a pull: the commit, the files, and how they will fit.
pub async fn plan(
    hub: &Hub,
    r: &ModelRef,
    hw: &Accelerators,
    gpu: bool,
    context: u64,
) -> Result<Plan, PlanError> {
    let commit = hub.commit(&r.repo, &r.revision).await?;
    let all = hub.files(&r.repo, &commit).await?;
    let ggufs: Vec<&RepoFile> = all
        .iter()
        .filter(|f| f.path.to_ascii_lowercase().ends_with(".gguf") && !secondary_gguf(&f.path))
        .collect();
    if !ggufs.is_empty() {
        // Each candidate's size includes its other parts.
        let sized: Vec<(String, u64)> = ggufs
            .iter()
            .map(|f| {
                (
                    f.path.clone(),
                    parts_of(&f.path, &all).iter().map(|p| p.size).sum(),
                )
            })
            .collect();
        let chosen: Option<String> = match &r.selector {
            Some(sel) => {
                let s = sel.to_ascii_lowercase();
                let exact = sized.iter().find(|(p, _)| p.to_ascii_lowercase() == s);
                let by_quant = sized.iter().find(|(p, _)| {
                    fit::quant_label(p).is_some_and(|q| q.to_ascii_lowercase() == s)
                });
                let by_name = sized
                    .iter()
                    .find(|(p, _)| p.to_ascii_lowercase().contains(&s));
                match exact.or(by_quant).or(by_name) {
                    Some((p, _)) => Some(p.clone()),
                    None => {
                        let names: Vec<&str> = sized.iter().map(|(p, _)| p.as_str()).collect();
                        return Err(PlanError::Choice(format!(
                            "{} has no GGUF file matching `{sel}`; it has: {}",
                            r.repo,
                            names.join(", ")
                        )));
                    }
                }
            }
            None => None,
        };
        // The shape, from one file's header (the files share it).
        let probe = chosen.clone().unwrap_or_else(|| sized[0].0.clone());
        let shape = remote_header(hub, &r.repo, &commit, &probe)
            .await
            .and_then(|h| h.shape());
        let choices = fit::rank(&sized, shape.as_ref(), context, hw, gpu);
        let pick = match chosen {
            Some(p) => p,
            None => match choices.first() {
                Some(c) if c.placement != fit::Placement::TooBig => c.file.clone(),
                _ => {
                    return Err(PlanError::Choice(format!(
                        "none of {}'s files fits this machine: pick one with {}:<quant> to fetch it anyway",
                        r.repo, r.repo
                    )));
                }
            },
        };
        let choice = choices.iter().find(|c| c.file == pick).cloned();
        let files: Vec<RepoFile> = parts_of(&pick, &all).into_iter().cloned().collect();
        return Ok(Plan {
            repo: r.repo.clone(),
            revision: r.revision.clone(),
            commit,
            estimate: choice.as_ref().map(|c| c.estimate),
            placement: choice.as_ref().map(|c| c.placement),
            files,
            kind: Kind::Gguf { file: pick },
            choices,
        });
    }
    // Safetensors: an embedding model.
    let has = |p: &str| all.iter().any(|f| f.path == p);
    if has("config.json") && all.iter().any(|f| f.path.ends_with(".safetensors")) {
        let mut files: Vec<RepoFile> = all
            .iter()
            .filter(|f| {
                EMBEDDING_FILES.contains(&f.path.as_str())
                    || (f.path.ends_with(".safetensors") && !f.path.contains('/'))
                    || f.path == "model.safetensors.index.json"
            })
            .cloned()
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let weights: u64 = files
            .iter()
            .filter(|f| f.path.ends_with(".safetensors"))
            .map(|f| f.size)
            .sum();
        let estimate = Estimate::new(weights, None, context);
        return Ok(Plan {
            repo: r.repo.clone(),
            revision: r.revision.clone(),
            commit,
            placement: Some(fit::place(&estimate, hw, false)),
            estimate: Some(estimate),
            files,
            kind: Kind::Embedding,
            choices: Vec::new(),
        });
    }
    Err(PlanError::Choice(format!(
        "{} has neither GGUF files nor safetensors weights with a config.json",
        r.repo
    )))
}

/// Every runnable model in the cache, with its id.
pub fn scan(
    cache: &Cache,
    servers: &std::collections::BTreeMap<String, termos_core::config::models::ServerConfig>,
) -> Vec<LocalModel> {
    let mut out = Vec::new();
    for CachedRepo {
        repo, snapshots, ..
    } in cache.repos()
    {
        // The most recently used snapshot of each repository.
        let Some(s) = snapshots.first() else { continue };
        let server = servers
            .iter()
            .find(|(_, c)| c.models.iter().any(|m| m == &repo))
            .map(|(n, _)| n.clone());
        if let Some(name) = server {
            out.push(LocalModel {
                id: repo.clone(),
                repo: repo.clone(),
                commit: s.commit.clone(),
                dir: s.dir.clone(),
                kind: Kind::Server { name },
                size: s.size(),
            });
            continue;
        }
        let ggufs: Vec<&(u64, String)> = s
            .files
            .iter()
            .filter(|(_, p)| p.to_ascii_lowercase().ends_with(".gguf") && !secondary_gguf(p))
            .collect();
        if !ggufs.is_empty() {
            let many = ggufs.len() > 1;
            for (size, path) in ggufs {
                let id = if many {
                    format!(
                        "{repo}:{}",
                        fit::quant_label(path)
                            .map_or_else(|| path.clone(), |q| q.to_ascii_lowercase())
                    )
                } else {
                    repo.clone()
                };
                let total: u64 = match split_part(&path.to_ascii_lowercase()) {
                    Some(_) => {
                        let prefix = &path[..path.len() - "-00001-of-00000.gguf".len()];
                        s.files
                            .iter()
                            .filter(|(_, p)| {
                                p.starts_with(prefix)
                                    && split_part(&p.to_ascii_lowercase()).is_some()
                            })
                            .map(|(sz, _)| *sz)
                            .sum()
                    }
                    None => *size,
                };
                out.push(LocalModel {
                    id,
                    repo: repo.clone(),
                    commit: s.commit.clone(),
                    dir: s.dir.clone(),
                    kind: Kind::Gguf { file: path.clone() },
                    size: total,
                });
            }
            continue;
        }
        let config = std::fs::read(s.dir.join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        if config.as_ref().is_some_and(is_bert) && s.dir.join("tokenizer.json").exists() {
            out.push(LocalModel {
                id: repo.clone(),
                repo: repo.clone(),
                commit: s.commit.clone(),
                dir: s.dir.clone(),
                kind: Kind::Embedding,
                size: s.size(),
            });
        }
    }
    out
}

/// Finds a model by id, or by repository when it has one model.
pub fn find<'a>(models: &'a [LocalModel], id: &str) -> Option<&'a LocalModel> {
    models.iter().find(|m| m.id == id).or_else(|| {
        let mut same = models.iter().filter(|m| m.repo == id);
        let first = same.next()?;
        same.next().is_none().then_some(first)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_and_split_files() {
        let r = ModelRef::parse("Qwen/Qwen2.5-0.5B-Instruct-GGUF:q4_k_m", None).expect("ref");
        assert_eq!(
            (r.repo.as_str(), r.selector.as_deref(), r.revision.as_str()),
            ("Qwen/Qwen2.5-0.5B-Instruct-GGUF", Some("q4_k_m"), "main")
        );
        let r = ModelRef::parse("ggml-org/models:tinyllamas/stories260K.gguf", Some("v1"))
            .expect("ref");
        assert_eq!(r.selector.as_deref(), Some("tinyllamas/stories260K.gguf"));
        assert!(ModelRef::parse("../x", None).is_err());
        assert_eq!(split_part("m-q8_0-00002-of-00003.gguf"), Some((2, 3)));
        assert!(secondary_gguf("m-Q8_0-00002-of-00003.gguf"));
        assert!(!secondary_gguf("m-Q8_0-00001-of-00003.gguf"));
        assert!(secondary_gguf("mmproj-model-f16.gguf"));
        let f = |p: &str| RepoFile {
            path: p.into(),
            size: 1,
            sha256: None,
            git_oid: String::new(),
        };
        let files = vec![
            f("m-Q8_0-00001-of-00002.gguf"),
            f("m-Q8_0-00002-of-00002.gguf"),
            f("m-Q4_0.gguf"),
        ];
        assert_eq!(parts_of("m-Q8_0-00001-of-00002.gguf", &files).len(), 2);
        assert_eq!(parts_of("m-Q4_0.gguf", &files).len(), 1);
    }

    #[test]
    fn config_shapes() {
        let qwen: Value = serde_json::json!({
            "architectures": ["Qwen2ForCausalLM"], "num_hidden_layers": 24, "num_attention_heads": 14,
            "num_key_value_heads": 2, "hidden_size": 896, "max_position_embeddings": 32768
        });
        let s = config_shape(&qwen).expect("shape");
        assert_eq!(
            (s.layers, s.kv_heads, s.key_dim, s.context),
            (24, 2, 64, Some(32768))
        );
        assert!(!is_bert(&qwen));
        assert!(is_bert(
            &serde_json::json!({"architectures": ["BertModel"]})
        ));
        assert!(is_bert(&serde_json::json!({"model_type": "bert"})));
    }
}
