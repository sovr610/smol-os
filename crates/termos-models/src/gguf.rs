// SPDX-License-Identifier: GPL-3.0-or-later
//! GGUF metadata: what the fit estimate and the backends need from a
//! model file's header (versions 2 and 3).
//!
//! The header is read from the start of a file, local or fetched with an
//! HTTP range request before downloading, so the parser works on a prefix
//! and says how many more bytes it needs. Arrays (the tokenizer's
//! vocabulary) are skipped, not kept. Fuzzed by `fuzz/fuzz_targets/gguf_parse.rs`.

use std::collections::BTreeMap;

/// A metadata value (arrays are only counted).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    /// An array: its element type and length.
    Array {
        kind: u32,
        len: u64,
    },
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(n) => Some(*n),
            Self::I64(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// A parsed header.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Header {
    pub version: u32,
    pub tensors: u64,
    pub meta: BTreeMap<String, Value>,
}

/// Why a header could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GgufError {
    /// More bytes are needed: at least this many in all.
    #[error("the header needs at least {0} bytes")]
    Incomplete(u64),
    #[error("not a GGUF file")]
    NotGguf,
    #[error("GGUF version {0} is not supported")]
    Version(u32),
    #[error("a malformed header: {0}")]
    Malformed(&'static str),
}

/// The most bytes a header may take (tokenizer vocabularies make headers
/// of several MiB).
pub const MAX_HEADER: u64 = 64 << 20;
/// Longest string kept (a chat template is a few KiB).
const MAX_STRING: u64 = 1 << 20;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: u64) -> Result<&[u8], GgufError> {
        let end = (self.pos as u64)
            .checked_add(n)
            .ok_or(GgufError::Malformed("a length overflows"))?;
        if end > MAX_HEADER {
            return Err(GgufError::Malformed("the header is too large"));
        }
        let end_usize = usize::try_from(end).map_err(|_| GgufError::Malformed("too large"))?;
        if end_usize > self.buf.len() {
            return Err(GgufError::Incomplete(end));
        }
        let s = &self.buf[self.pos..end_usize];
        self.pos = end_usize;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, GgufError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, GgufError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    fn string(&mut self) -> Result<String, GgufError> {
        let len = self.u64()?;
        if len > MAX_STRING {
            // Skip it (still within the header limit).
            self.take(len)?;
            return Ok(String::new());
        }
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    /// The size of one scalar of `kind`, `None` for strings and arrays.
    fn scalar_size(kind: u32) -> Option<u64> {
        match kind {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        }
    }

    fn value(&mut self, kind: u32, depth: u32) -> Result<Value, GgufError> {
        let le = |b: &[u8]| {
            let mut a = [0u8; 8];
            a[..b.len()].copy_from_slice(b);
            u64::from_le_bytes(a)
        };
        Ok(match kind {
            0 => Value::U64(u64::from(self.take(1)?[0])),
            1 => Value::I64(i64::from(self.take(1)?[0] as i8)),
            2 => Value::U64(le(self.take(2)?)),
            3 => Value::I64(i64::from(le(self.take(2)?) as u16 as i16)),
            4 => Value::U64(le(self.take(4)?)),
            5 => Value::I64(i64::from(le(self.take(4)?) as u32 as i32)),
            6 => Value::F64(f64::from(f32::from_bits(le(self.take(4)?) as u32))),
            7 => Value::Bool(self.take(1)?[0] != 0),
            8 => Value::Str(self.string()?),
            9 => {
                if depth > 2 {
                    return Err(GgufError::Malformed("arrays nested too deeply"));
                }
                let elem = self.u32()?;
                let len = self.u64()?;
                match Self::scalar_size(elem) {
                    Some(sz) => {
                        self.take(
                            len.checked_mul(sz)
                                .ok_or(GgufError::Malformed("an array overflows"))?,
                        )?;
                    }
                    None if elem == 8 || elem == 9 => {
                        for _ in 0..len {
                            self.value(elem, depth + 1)?;
                        }
                    }
                    None => return Err(GgufError::Malformed("an unknown array type")),
                }
                Value::Array { kind: elem, len }
            }
            10 => Value::U64(le(self.take(8)?)),
            11 => Value::I64(le(self.take(8)?) as i64),
            12 => Value::F64(f64::from_bits(le(self.take(8)?))),
            _ => return Err(GgufError::Malformed("an unknown value type")),
        })
    }
}

/// Parses the header at the start of `buf`.
pub fn parse(buf: &[u8]) -> Result<Header, GgufError> {
    let mut r = Reader { buf, pos: 0 };
    let magic = r.take(4)?;
    if magic != b"GGUF" {
        return Err(GgufError::NotGguf);
    }
    let version = r.u32()?;
    if !(2..=3).contains(&version) {
        return Err(GgufError::Version(version));
    }
    let tensors = r.u64()?;
    let count = r.u64()?;
    if count > 100_000 {
        return Err(GgufError::Malformed("too many metadata entries"));
    }
    let mut meta = BTreeMap::new();
    for _ in 0..count {
        let key = r.string()?;
        let kind = r.u32()?;
        let value = r.value(kind, 0)?;
        meta.insert(key, value);
    }
    Ok(Header {
        version,
        tensors,
        meta,
    })
}

impl Header {
    fn get(&self, key: &str) -> Option<&Value> {
        self.meta.get(key)
    }

    /// `general.architecture` (`llama`, `qwen2`, `gemma3`…).
    pub fn architecture(&self) -> Option<&str> {
        self.get("general.architecture").and_then(Value::as_str)
    }

    /// `<architecture>.<key>` as a number.
    pub fn arch_u64(&self, key: &str) -> Option<u64> {
        let arch = self.architecture()?;
        self.get(&format!("{arch}.{key}")).and_then(Value::as_u64)
    }

    pub fn name(&self) -> Option<&str> {
        self.get("general.name").and_then(Value::as_str)
    }

    /// Whether the file carries a chat template.
    pub fn has_chat_template(&self) -> bool {
        self.get("tokenizer.chat_template")
            .and_then(Value::as_str)
            .is_some_and(|t| !t.is_empty())
    }

    /// The shape the fit estimate needs.
    pub fn shape(&self) -> Option<crate::fit::Shape> {
        let layers = self.arch_u64("block_count")?;
        let heads = self.arch_u64("attention.head_count")?;
        let kv_heads = self.arch_u64("attention.head_count_kv").unwrap_or(heads);
        let embed = self.arch_u64("embedding_length")?;
        let head_dim = embed.checked_div(heads).unwrap_or(0);
        Some(crate::fit::Shape {
            layers,
            kv_heads,
            key_dim: self.arch_u64("attention.key_length").unwrap_or(head_dim),
            value_dim: self.arch_u64("attention.value_length").unwrap_or(head_dim),
            context: self.arch_u64("context_length"),
        })
    }
}

/// Builds a GGUF header (tests, and the fuzz seed corpus).
pub fn build(entries: &[(&str, Value)], tensors: u64) -> Vec<u8> {
    let mut out = b"GGUF".to_vec();
    out.extend(3u32.to_le_bytes());
    out.extend(tensors.to_le_bytes());
    out.extend((entries.len() as u64).to_le_bytes());
    for (k, v) in entries {
        out.extend((k.len() as u64).to_le_bytes());
        out.extend(k.as_bytes());
        match v {
            Value::U64(n) => {
                out.extend(10u32.to_le_bytes());
                out.extend(n.to_le_bytes());
            }
            Value::I64(n) => {
                out.extend(11u32.to_le_bytes());
                out.extend(n.to_le_bytes());
            }
            Value::F64(n) => {
                out.extend(12u32.to_le_bytes());
                out.extend(n.to_le_bytes());
            }
            Value::Bool(b) => {
                out.extend(7u32.to_le_bytes());
                out.push(u8::from(*b));
            }
            Value::Str(s) => {
                out.extend(8u32.to_le_bytes());
                out.extend((s.len() as u64).to_le_bytes());
                out.extend(s.as_bytes());
            }
            Value::Array { kind, len } => {
                // An array of `len` one-character strings, or of zeroed
                // scalars.
                out.extend(9u32.to_le_bytes());
                out.extend(kind.to_le_bytes());
                out.extend(len.to_le_bytes());
                for _ in 0..*len {
                    if *kind == 8 {
                        out.extend(1u64.to_le_bytes());
                        out.push(b'a');
                    } else {
                        let sz = Reader::scalar_size(*kind).unwrap_or(1);
                        out.extend(std::iter::repeat_n(0u8, sz as usize));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metadata of a Llama-3-8B-like model (the shape published in its
    /// GGUF files).
    pub(crate) fn llama3_8b() -> Vec<u8> {
        build(
            &[
                ("general.architecture", Value::Str("llama".into())),
                (
                    "general.name",
                    Value::Str("Meta Llama 3 8B Instruct".into()),
                ),
                ("llama.block_count", Value::U64(32)),
                ("llama.context_length", Value::U64(8192)),
                ("llama.embedding_length", Value::U64(4096)),
                ("llama.attention.head_count", Value::U64(32)),
                ("llama.attention.head_count_kv", Value::U64(8)),
                ("tokenizer.ggml.tokens", Value::Array { kind: 8, len: 300 }),
                ("tokenizer.ggml.scores", Value::Array { kind: 6, len: 300 }),
                (
                    "tokenizer.chat_template",
                    Value::Str("{{ messages }}".into()),
                ),
            ],
            291,
        )
    }

    #[test]
    fn a_header_and_its_shape() {
        let h = parse(&llama3_8b()).expect("parses");
        assert_eq!(h.version, 3);
        assert_eq!(h.tensors, 291);
        assert_eq!(h.architecture(), Some("llama"));
        assert_eq!(h.name(), Some("Meta Llama 3 8B Instruct"));
        assert!(h.has_chat_template());
        assert_eq!(
            h.meta["tokenizer.ggml.tokens"],
            Value::Array { kind: 8, len: 300 }
        );
        let s = h.shape().expect("shape");
        assert_eq!(
            (s.layers, s.kv_heads, s.key_dim, s.value_dim, s.context),
            (32, 8, 128, 128, Some(8192))
        );
    }

    #[test]
    fn prefixes_ask_for_more_and_garbage_is_refused() {
        let full = llama3_8b();
        for cut in [0, 3, 10, 24, 40, full.len() / 2, full.len() - 1] {
            match parse(&full[..cut]) {
                Err(GgufError::Incomplete(n)) => {
                    assert!(n as usize > cut && n as usize <= full.len(), "{cut} → {n}")
                }
                other => panic!("{cut}: {other:?}"),
            }
        }
        assert_eq!(parse(b"GGML1234"), Err(GgufError::NotGguf));
        let mut v1 = full.clone();
        v1[4] = 1;
        assert_eq!(parse(&v1), Err(GgufError::Version(1)));
        // An absurd array length is refused, not allocated.
        let huge = build(&[("x", Value::Array { kind: 10, len: 0 })], 0);
        let mut huge = huge;
        let at = huge.len() - 8;
        huge[at..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(parse(&huge), Err(GgufError::Malformed(_))));
    }
}
