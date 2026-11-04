// SPDX-License-Identifier: GPL-3.0-or-later
//! What headless Claude Code and Codex CLI print, one JSON object a line
//! (D-100).
//!
//! `claude -p --output-format stream-json --verbose` prints its system
//! init, each assistant message (text and tool calls), tool results and a
//! final `result` with the answer, tokens and an estimated cost. `codex
//! exec --json` prints thread and turn events and items (messages,
//! commands, file changes), with tokens on `turn.completed`. These are the
//! CLIs' formats, not TermOS's: only the fields needed are read, and
//! anything else is skipped.

use serde_json::Value;

/// Tokens a task used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn add(&mut self, o: Self) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }

    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Something a CLI reported.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Its session (to resume it) and model.
    Started {
        session: Option<String>,
        model: Option<String>,
    },
    /// Text from the model.
    Text(String),
    /// A tool it used: its name and what on (a file, a command).
    Tool {
        name: String,
        detail: String,
    },
    Usage(Tokens),
    /// Claude Code's last line: the answer, or the error.
    Result {
        text: String,
        error: bool,
        /// Claude Code's estimate (US dollars); not a charge under a plan.
        cost: Option<f64>,
        tokens: Tokens,
        /// Tools it was not allowed to use.
        denied: Vec<String>,
    },
    /// Codex's turn or stream failed.
    Failed(String),
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// What a tool call was about: its file, command or pattern.
fn tool_detail(input: &Value) -> String {
    for key in [
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
    ] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            return one_line(s, 100);
        }
    }
    String::new()
}

/// The first line of `s`, at most `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    let mut out: String = first.chars().take(max).collect();
    if first.chars().count() > max || s.lines().nth(1).is_some() {
        out.push('…');
    }
    out
}

/// One line of `claude -p --output-format stream-json`.
pub fn claude_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            vec![Event::Started {
                session: text(&v, "session_id"),
                model: text(&v, "model"),
            }]
        }
        Some("assistant") => {
            let mut out = Vec::new();
            let content = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = text(&block, "text") {
                            out.push(Event::Text(t));
                        }
                    }
                    Some("tool_use") => out.push(Event::Tool {
                        name: text(&block, "name").unwrap_or_else(|| "tool".into()),
                        detail: block.get("input").map(tool_detail).unwrap_or_default(),
                    }),
                    _ => {}
                }
            }
            out
        }
        Some("result") => {
            let u = v.get("usage").cloned().unwrap_or(Value::Null);
            let tokens = Tokens {
                input: n(&u, "input_tokens"),
                output: n(&u, "output_tokens"),
                cache_read: n(&u, "cache_read_input_tokens"),
                cache_write: n(&u, "cache_creation_input_tokens"),
            };
            let error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                || v.get("subtype")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("error"));
            let denied = v
                .get("permission_denials")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|d| {
                            let name = text(d, "tool_name").unwrap_or_else(|| "tool".into());
                            match d.get("tool_input").map(tool_detail) {
                                Some(detail) if !detail.is_empty() => format!("{name} {detail}"),
                                _ => name,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut answer = text(&v, "result").unwrap_or_default();
            if answer.is_empty() && error {
                answer = text(&v, "subtype").unwrap_or_else(|| "the task failed".into());
            }
            vec![Event::Result {
                text: answer,
                error,
                cost: v.get("total_cost_usd").and_then(Value::as_f64),
                tokens,
                denied,
            }]
        }
        _ => Vec::new(),
    }
}

/// One line of `codex exec --json`.
pub fn codex_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match v.get("type").and_then(Value::as_str) {
        Some("thread.started") => vec![Event::Started {
            session: text(&v, "thread_id"),
            model: None,
        }],
        Some("item.started" | "item.completed") => {
            let done = v.get("type").and_then(Value::as_str) == Some("item.completed");
            let item = v.get("item").cloned().unwrap_or(Value::Null);
            match (item.get("type").and_then(Value::as_str), done) {
                (Some("agent_message"), true) => text(&item, "text")
                    .map(|t| vec![Event::Text(t)])
                    .unwrap_or_default(),
                (Some("command_execution"), false) => vec![Event::Tool {
                    name: "command".into(),
                    detail: item
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|c| one_line(c, 100))
                        .unwrap_or_default(),
                }],
                (Some("file_change"), true) => {
                    let paths: Vec<String> = item
                        .get("changes")
                        .and_then(Value::as_array)
                        .map(|c| c.iter().filter_map(|c| text(c, "path")).collect())
                        .unwrap_or_default();
                    vec![Event::Tool {
                        name: "edit".into(),
                        detail: one_line(&paths.join(", "), 100),
                    }]
                }
                (Some(kind @ ("mcp_tool_call" | "web_search")), false) => vec![Event::Tool {
                    name: kind.replace('_', " "),
                    detail: text(&item, "tool")
                        .or_else(|| text(&item, "query"))
                        .unwrap_or_default(),
                }],
                (Some("error"), _) => text(&item, "message")
                    .map(|m| vec![Event::Failed(m)])
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        Some("turn.completed") => {
            let u = v.get("usage").cloned().unwrap_or(Value::Null);
            let cached = n(&u, "cached_input_tokens");
            vec![Event::Usage(Tokens {
                // Codex counts cached input inside its input tokens.
                input: n(&u, "input_tokens").saturating_sub(cached),
                output: n(&u, "output_tokens"),
                cache_read: cached,
                cache_write: 0,
            })]
        }
        Some("turn.failed") => vec![Event::Failed(
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the turn failed")
                .to_owned(),
        )],
        Some("error") => vec![Event::Failed(
            text(&v, "message").unwrap_or_else(|| "an error".into()),
        )],
        _ => Vec::new(),
    }
}

/// Whether a failure is a plan's usage or rate limit, so the harness
/// stops using that CLI for the rest of the plan (and never moves to
/// another account of it, D-096).
pub fn is_limit(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "usage limit",
        "rate limit",
        "limit reached",
        "hit your limit",
        "reached your limit",
        "too many requests",
        "429",
        "quota",
        "try again in",
    ]
    .iter()
    .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines as the CLIs print them, rebuilt from their documented formats.
    const CLAUDE: &str = r#"{"type":"system","subtype":"init","session_id":"s-1","model":"fixture-model","tools":["Read","Edit"]}
{"type":"assistant","message":{"content":[{"type":"text","text":"Looking at the client."},{"type":"tool_use","id":"u1","name":"Edit","input":{"file_path":"src/net.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"u1","content":"ok"}]}}
not json at all
{"type":"result","subtype":"success","is_error":false,"result":"Added retries.","total_cost_usd":0.042,"session_id":"s-1","usage":{"input_tokens":120,"cache_creation_input_tokens":300,"cache_read_input_tokens":900,"output_tokens":45},"permission_denials":[{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}]}"#;

    const CODEX: &str = r#"{"type":"thread.started","thread_id":"th-9"}
{"type":"turn.started"}
{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"bash -lc 'cargo test'","status":"in_progress"}}
{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"bash -lc 'cargo test'","exit_code":0}}
{"type":"item.completed","item":{"id":"i2","type":"file_change","changes":[{"path":"src/lib.rs","kind":"update"}]}}
{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"Fixed the test."}}
{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}"#;

    #[test]
    fn claude_codes_stream() {
        let events: Vec<Event> = CLAUDE.lines().flat_map(claude_line).collect();
        assert_eq!(
            events[0],
            Event::Started {
                session: Some("s-1".into()),
                model: Some("fixture-model".into())
            }
        );
        assert_eq!(events[1], Event::Text("Looking at the client.".into()));
        assert_eq!(
            events[2],
            Event::Tool {
                name: "Edit".into(),
                detail: "src/net.rs".into()
            }
        );
        match &events[3] {
            Event::Result {
                text,
                error,
                cost,
                tokens,
                denied,
            } => {
                assert_eq!(text, "Added retries.");
                assert!(!error);
                assert_eq!(*cost, Some(0.042));
                assert_eq!(tokens.total(), 1365);
                assert_eq!(denied, &["Bash rm -rf build"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(events.len(), 4);
        let failed = claude_line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Claude AI usage limit reached|1800000000"}"#,
        );
        match &failed[0] {
            Event::Result { text, error, .. } => {
                assert!(*error && is_limit(text));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn codexs_stream() {
        let events: Vec<Event> = CODEX.lines().flat_map(codex_line).collect();
        assert_eq!(
            events,
            [
                Event::Started {
                    session: Some("th-9".into()),
                    model: None
                },
                Event::Tool {
                    name: "command".into(),
                    detail: "bash -lc 'cargo test'".into()
                },
                Event::Tool {
                    name: "edit".into(),
                    detail: "src/lib.rs".into()
                },
                Event::Text("Fixed the test.".into()),
                Event::Usage(Tokens {
                    input: 200,
                    output: 50,
                    cache_read: 800,
                    cache_write: 0
                }),
            ]
        );
        assert_eq!(
            codex_line(
                r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again in 3 days."}}"#
            ),
            [Event::Failed(
                "You've hit your usage limit. Try again in 3 days.".into()
            )]
        );
        assert!(is_limit("stream error: 429 Too Many Requests"));
        assert!(!is_limit("cargo test failed: 3 tests"));
    }
}
