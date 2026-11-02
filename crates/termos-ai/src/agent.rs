// SPDX-License-Identifier: GPL-3.0-or-later
//! An agent session: the loop behind an agent window (ARCHITECTURE.md
//! §11, D-059).
//!
//! Each window runs its session on a thread of its own with a
//! single-threaded tokio runtime, so a slow provider or a long command
//! never stalls the desktop, and a closed window leaves nothing behind.
//! The loop: stream the model's reply → collect its tool calls → ask the
//! permission engine → ask the user when it says so → run the call (in
//! the sandbox) → send the results back → repeat until the model stops.
//! Esc cancels whatever is running. The window and the loop share a
//! [`View`]; the loop changes it and wakes the window.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::SecretString;
use termos_core::app::AppWaker;
use termos_core::config::agents::{AgentsConfig, Price, ProfileConfig, SecretStoreKind};
use termos_core::paths::Paths;
use termos_term::{AlacrittyVt, VtBackend};
use tokio::sync::mpsc;

use crate::audit::{self, AuditLog, Event};
use crate::history::{self, Saved};
use crate::mcp;
use crate::policy::{Decision, Policy, SessionApprovals};
use crate::provider::{
    self, Block, ChatRequest, Delta, Message, ModelInfo, Provider, ProviderError, Reply, Role,
    StopReason, Usage,
};
use crate::redact::Redactor;
use crate::sandbox;
use crate::secrets::{SecretError, SecretStore};
use crate::tools::{Call, Output, Preview, SandboxSetup, Toolbox};

/// Rounds of tool calls one message may lead to before the loop stops
/// and asks the user to continue.
const MAX_ROUNDS: usize = 60;
/// Tries for a request the provider could not take right now.
const TRIES: u32 = 4;
/// Size of a command's terminal in the window.
pub const TERM_COLS: u16 = 100;
pub const TERM_ROWS: u16 = 12;

/// What the window shows. Changed by the session, read by the window.
#[derive(Debug)]
pub struct View {
    pub phase: Phase,
    pub items: Vec<Item>,
    /// What is happening now ("thinking…", "retrying in 4s").
    pub status: String,
    pub provider: String,
    pub model: String,
    pub profile: String,
    pub workspace: PathBuf,
    pub usage: Usage,
    /// US dollars so far, when the model's prices are known.
    pub cost: Option<f64>,
    pub sandbox: String,
    pub session_id: String,
    /// Bumped on every change.
    pub generation: u64,
}

impl View {
    fn new(profile: &str, workspace: PathBuf) -> Self {
        Self {
            phase: Phase::Starting,
            items: Vec::new(),
            status: "starting…".into(),
            provider: String::new(),
            model: String::new(),
            profile: profile.to_owned(),
            workspace,
            usage: Usage::default(),
            cost: None,
            sandbox: String::new(),
            session_id: String::new(),
            generation: 0,
        }
    }

    /// The text item being streamed, started if needed.
    fn streaming_text(&mut self, thinking: bool) -> &mut String {
        let open = matches!(
            (self.items.last(), thinking),
            (Some(Item::Assistant { done: false, .. }), false)
                | (Some(Item::Thinking { done: false, .. }), true)
        );
        if !open {
            self.items.push(if thinking {
                Item::Thinking {
                    text: String::new(),
                    done: false,
                }
            } else {
                Item::Assistant {
                    text: String::new(),
                    done: false,
                }
            });
        }
        match self.items.last_mut() {
            Some(Item::Assistant { text, .. } | Item::Thinking { text, .. }) => text,
            _ => unreachable!("an open text item was just ensured"),
        }
    }

    fn close_streams(&mut self) {
        for item in &mut self.items {
            if let Item::Assistant { done, .. } | Item::Thinking { done, .. } = item {
                *done = true;
            }
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(&mut **t),
            _ => None,
        })
    }
}

/// Where the session is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Starting,
    /// The secrets file must be unlocked.
    NeedPassphrase {
        error: Option<String>,
    },
    /// Several providers and no profile naming one.
    PickProvider {
        names: Vec<String>,
    },
    /// Pick a model from the provider's list (or type one when the list
    /// could not be fetched: `error` says why).
    PickModel {
        models: Vec<ModelInfo>,
        error: Option<String>,
    },
    /// Waiting for a message.
    Ready,
    /// A turn is running.
    Busy,
    /// A tool call waits for the user.
    Confirm(Pending),
    /// The session cannot go on.
    Failed(String),
}

/// A tool call waiting for approval.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub call_id: String,
    pub tool: String,
    /// One line about the call (`cargo test`, `edit src/lib.rs`).
    pub summary: String,
    /// Why it asks.
    pub why: String,
    /// "Allow for this session" may be offered.
    pub remember: bool,
    pub preview: Preview,
}

/// One entry of the transcript.
#[derive(Debug)]
pub enum Item {
    User(String),
    Assistant { text: String, done: bool },
    Thinking { text: String, done: bool },
    Tool(Box<ToolItem>),
    Notice(String),
    Error(String),
}

#[derive(Debug)]
pub struct ToolItem {
    pub id: String,
    pub tool: String,
    pub summary: String,
    pub state: ToolState,
    /// The first lines of the result.
    pub result: Option<String>,
    /// A running or finished command's terminal.
    pub term: Option<AlacrittyVt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    Asking,
    Running,
    Done,
    Failed,
    Denied(String),
    Cancelled,
}

/// What the window sends the session.
#[derive(Debug)]
pub enum Input {
    Send(String),
    /// Approve the pending call (`remember`: for the rest of the session).
    Approve {
        remember: bool,
    },
    Deny,
    Cancel,
    Passphrase(SecretString),
    Provider(String),
    Model(String),
    Close,
}

/// Everything a session needs to start.
#[derive(Clone)]
pub struct Launch {
    pub agents: AgentsConfig,
    pub secrets: Arc<SecretSlot>,
    pub paths: Paths,
    pub profile: String,
    pub workspace: PathBuf,
    /// Resume this saved conversation.
    pub resume: Option<String>,
    /// The `termos` binary (the sandbox helper).
    pub helper: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("profile", &self.profile)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// The server's secret store, opened on first use (opening may ask the
/// keyring service, a D-Bus call) and shared by every window, so the
/// secrets file is unlocked once.
pub struct SecretSlot {
    kind: SecretStoreKind,
    data_dir: PathBuf,
    store: std::sync::OnceLock<Arc<SecretStore>>,
}

impl SecretSlot {
    pub fn new(kind: SecretStoreKind, data_dir: PathBuf) -> Self {
        Self {
            kind,
            data_dir,
            store: std::sync::OnceLock::new(),
        }
    }

    /// A slot holding an already opened store (tests).
    pub fn with(store: SecretStore) -> Self {
        let slot = Self::new(SecretStoreKind::File, PathBuf::new());
        let _ = slot.store.set(Arc::new(store));
        slot
    }

    pub fn get(&self) -> Arc<SecretStore> {
        Arc::clone(
            self.store
                .get_or_init(|| Arc::new(SecretStore::open(self.kind, &self.data_dir))),
        )
    }
}

/// The window's end of a session.
#[derive(Clone)]
pub struct Handle {
    pub view: Arc<Mutex<View>>,
    pub input: mpsc::UnboundedSender<Input>,
    waker: Arc<Mutex<Option<AppWaker>>>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("agent::Handle")
    }
}

impl Handle {
    pub fn set_waker(&self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
    }

    pub fn send(&self, input: Input) {
        let _ = self.input.send(input);
    }
}

/// Starts a session on its own thread.
pub fn start(launch: Launch) -> std::io::Result<Handle> {
    let view = Arc::new(Mutex::new(View::new(
        &launch.profile,
        launch.workspace.clone(),
    )));
    let (tx, rx) = mpsc::unbounded_channel();
    let waker: Arc<Mutex<Option<AppWaker>>> = Arc::new(Mutex::new(None));
    let shared = Shared {
        view: Arc::clone(&view),
        waker: Arc::clone(&waker),
    };
    std::thread::Builder::new()
        .name("termos-agent".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    shared.update(|v| v.phase = Phase::Failed(format!("cannot start: {e}")));
                    return;
                }
            };
            rt.block_on(run(launch, shared, rx));
        })?;
    Ok(Handle {
        view,
        input: tx,
        waker,
    })
}

/// The session's side of the shared view.
#[derive(Clone)]
pub struct Shared {
    view: Arc<Mutex<View>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
}

impl Shared {
    pub fn update(&self, f: impl FnOnce(&mut View)) {
        if let Ok(mut v) = self.view.lock() {
            f(&mut v);
            v.generation += 1;
        }
        if let Ok(w) = self.waker.lock()
            && let Some(w) = w.as_ref()
        {
            w.wake();
        }
    }

    fn phase(&self, phase: Phase, status: &str) {
        self.update(|v| {
            v.phase = phase;
            status.clone_into(&mut v.status);
        });
    }

    fn notice(&self, text: impl Into<String>) {
        let text = text.into();
        self.update(|v| v.items.push(Item::Notice(text)));
    }

    fn error(&self, text: impl Into<String>) {
        let text = text.into();
        self.update(|v| v.items.push(Item::Error(text)));
    }
}

/// Why a turn ended early.
enum Interrupt {
    Cancelled,
    Closed,
}

async fn run(launch: Launch, shared: Shared, mut rx: mpsc::UnboundedReceiver<Input>) {
    let mut session = match Session::begin(&launch, &shared, &mut rx).await {
        Ok(s) => s,
        Err(Some(why)) => {
            shared.phase(Phase::Failed(why), "");
            // Keep the thread until the window closes, so it can show why.
            while let Some(i) = rx.recv().await {
                if matches!(i, Input::Close) {
                    break;
                }
            }
            return;
        }
        Err(None) => return,
    };
    shared.phase(Phase::Ready, "");
    while let Some(input) = rx.recv().await {
        match input {
            Input::Send(text) => {
                shared.phase(Phase::Busy, "thinking…");
                match session.turn(text, &mut rx).await {
                    Ok(()) => shared.phase(Phase::Ready, ""),
                    Err(Interrupt::Cancelled) => {
                        shared.notice("cancelled");
                        shared.phase(Phase::Ready, "");
                    }
                    Err(Interrupt::Closed) => break,
                }
            }
            Input::Close => break,
            _ => {}
        }
    }
    session.end().await;
}

struct Session {
    shared: Shared,
    id: String,
    provider: Arc<dyn Provider>,
    provider_name: String,
    model: String,
    price: Option<Price>,
    system: String,
    tools: Toolbox,
    approvals: SessionApprovals,
    messages: Vec<Message>,
    usage: Usage,
    audit: Option<AuditLog>,
    history_dir: Option<PathBuf>,
    created: String,
    profile: String,
    workspace: PathBuf,
}

/// Waits for an input the predicate accepts; `None` when the window
/// closed.
async fn wait_for<T>(
    rx: &mut mpsc::UnboundedReceiver<Input>,
    mut accept: impl FnMut(Input) -> Option<T>,
) -> Option<T> {
    while let Some(i) = rx.recv().await {
        if matches!(i, Input::Close) {
            return None;
        }
        if let Some(t) = accept(i) {
            return Some(t);
        }
    }
    None
}

/// The system prompt: TermOS's instructions, then the profile's.
fn system_prompt(
    workspace: &std::path::Path,
    sandbox_line: &str,
    profile: &ProfileConfig,
) -> String {
    let mut s = format!(
        "You are an AI agent in TermOS, a terminal desktop, working on the user's project.\n\
         Workspace: {}. Relative paths start there. System: {} ({}).\n\
         Look before you change anything; keep changes small and say briefly what you did.\n\
         Every tool call goes through TermOS's permission engine, and the user may approve or refuse it. \
         If a call is refused, do not try to get around the refusal; ask the user instead.\n\
         Commands run in a sandbox ({sandbox_line}): the workspace is writable, /tmp is private, and there is \
         no network unless you set network=true on run_command, which the user must approve.",
        workspace.display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    if let Some(extra) = profile
        .instructions
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        s.push_str("\n\n");
        s.push_str(extra.trim());
    }
    s
}

impl Session {
    /// Resolves the profile, provider, key and model, then the sandbox,
    /// MCP servers and audit log. `Err(None)`: the window closed.
    async fn begin(
        launch: &Launch,
        shared: &Shared,
        rx: &mut mpsc::UnboundedReceiver<Input>,
    ) -> Result<Self, Option<String>> {
        let agents = &launch.agents;
        let profile = match agents.profiles.get(&launch.profile) {
            Some(p) => p.clone(),
            None if launch.profile == "default" => ProfileConfig::default(),
            None => {
                return Err(Some(format!(
                    "no agent profile named {} in [agents.profiles]",
                    launch.profile
                )));
            }
        };
        let resumed = match &launch.resume {
            Some(id) if agents.history => Some(
                Saved::load(&history::dir(&launch.paths.state_dir), id)
                    .map_err(|e| Some(format!("cannot resume {id}: {e}")))?,
            ),
            Some(_) => {
                return Err(Some(
                    "conversations are not kept ([agents] history = false)".into(),
                ));
            }
            None => None,
        };
        if agents.providers.is_empty() {
            return Err(Some(
                "no providers are configured: add one under [agents.providers.<name>] in config.toml \
                 (see docs/agents.md), store its key with `termos secret set <name>`, and open the agent again"
                    .into(),
            ));
        }
        // The provider.
        let provider_name = match (&resumed, &profile.provider) {
            (Some(s), _) => s.provider.clone(),
            (None, Some(p)) => p.clone(),
            (None, None) if agents.providers.len() == 1 => {
                agents.providers.keys().next().cloned().unwrap_or_default()
            }
            (None, None) => {
                let names: Vec<String> = agents.providers.keys().cloned().collect();
                shared.phase(
                    Phase::PickProvider {
                        names: names.clone(),
                    },
                    "pick a provider",
                );
                wait_for(rx, |i| match i {
                    Input::Provider(p) if names.contains(&p) => Some(p),
                    _ => None,
                })
                .await
                .ok_or(None)?
            }
        };
        let Some(pcfg) = agents.providers.get(&provider_name).cloned() else {
            return Err(Some(format!(
                "no provider named {provider_name} in [agents.providers]"
            )));
        };
        shared.update(|v| {
            v.provider.clone_from(&provider_name);
            v.status = "reading the API key…".into();
        });
        // Keys: the provider's and the MCP servers'.
        let store = launch.secrets.clone();
        let mut redactor = Redactor::new();
        let key = match &pcfg.key {
            Some(r) => {
                let k = read_secret(&store, r.name(), shared, rx).await?;
                redactor.add(&k);
                Some(k)
            }
            None => None,
        };
        let mut mcp_keys = BTreeMap::new();
        for name in &profile.mcp_servers {
            if let Some(r) = agents.mcp.get(name).and_then(|m| m.key.as_ref()) {
                match read_secret(&store, r.name(), shared, rx).await {
                    Ok(k) => {
                        redactor.add(&k);
                        mcp_keys.insert(name.clone(), k);
                    }
                    Err(None) => return Err(None),
                    Err(Some(e)) => shared.error(format!("MCP server {name}: {e}")),
                }
            }
        }
        let provider = provider::build(&provider_name, &pcfg, key).map_err(|e| Some(e.message))?;
        // The model.
        let (model, listed) = match (&resumed, profile.model.as_deref().filter(|m| !m.is_empty())) {
            (Some(s), _) => (s.model.clone(), None),
            (None, Some(m)) => (m.to_owned(), None),
            (None, None) => {
                shared.phase(Phase::Starting, "listing models…");
                let (models, error) = match provider.list_models().await {
                    Ok(m) if m.is_empty() => (
                        Vec::new(),
                        Some("the provider lists no models: type one".to_owned()),
                    ),
                    Ok(m) => (m, None),
                    Err(e) => (
                        Vec::new(),
                        Some(format!("could not list models ({e}): type one")),
                    ),
                };
                shared.phase(
                    Phase::PickModel {
                        models: models.clone(),
                        error,
                    },
                    "pick a model",
                );
                let model = wait_for(rx, |i| match i {
                    Input::Model(m) if !m.trim().is_empty() => Some(m.trim().to_owned()),
                    _ => None,
                })
                .await
                .ok_or(None)?;
                let info = models.into_iter().find(|m| m.id == model);
                (model, info)
            }
        };
        let price = provider::price_for(&pcfg, &model, listed.as_ref());
        // Sandbox, policy, tools.
        let caps = sandbox::probe(launch.helper.as_deref()).clone();
        let confinement = caps.confinement(profile.sandbox);
        let secrets_file = crate::secrets::file_path(&launch.paths.data_dir);
        let policy = Policy::new(
            &launch.profile,
            &profile,
            &launch.workspace,
            launch.home.as_deref(),
            vec![secrets_file.clone()],
            confinement,
        );
        let hidden = hidden_paths(launch, &secrets_file);
        let setup = SandboxSetup {
            caps: caps.clone(),
            helper: launch.helper.clone(),
            hidden,
        };
        let sandbox_line = caps.describe(profile.sandbox);
        let hub = if profile.mcp_servers.is_empty() {
            mcp::Hub::default()
        } else {
            shared.phase(Phase::Starting, "starting MCP servers…");
            mcp::Hub::start(
                &profile.mcp_servers,
                &agents.mcp,
                &mcp_keys,
                &mcp::Launch {
                    caps: &caps,
                    mode: profile.sandbox,
                    helper: launch.helper.as_deref(),
                    workspace: &policy.workspace,
                    hidden: &setup.hidden,
                },
            )
            .await
        };
        for p in &hub.problems {
            shared.error(p.clone());
        }
        let workspace = policy.workspace.clone();
        let system = system_prompt(&workspace, &sandbox_line, &profile);
        let tools = Toolbox {
            policy,
            sandbox: setup,
            redactor,
            mcp: hub,
        };
        // The session's identity, audit log and history.
        let (id, messages, usage, created) = match resumed {
            Some(s) => (s.id, s.messages, s.usage, s.created),
            None => (
                history::new_id(),
                Vec::new(),
                Usage::default(),
                jiff::Timestamp::now().to_string(),
            ),
        };
        let mut audit = match AuditLog::open(&audit::dir(&launch.paths.state_dir), &id) {
            Ok(a) => Some(a),
            Err(e) => {
                shared.error(format!(
                    "no audit log ({e}): tools will not run without one"
                ));
                None
            }
        };
        if let Some(a) = &mut audit {
            let _ = a.append(Event::Session {
                profile: launch.profile.clone(),
                provider: provider_name.clone(),
                model: model.clone(),
                workspace: workspace.display().to_string(),
                sandbox: sandbox_line.clone(),
            });
        }
        let history_dir = agents
            .history
            .then(|| history::dir(&launch.paths.state_dir));
        let cost = price.map(|p| usage.cost(&p));
        shared.update(|v| {
            v.model.clone_from(&model);
            v.sandbox.clone_from(&sandbox_line);
            v.session_id.clone_from(&id);
            v.usage = usage;
            v.cost = cost;
            v.workspace.clone_from(&workspace);
            for m in &messages {
                push_message_items(v, m);
            }
            if !messages.is_empty() {
                v.items.push(Item::Notice("resumed".into()));
            }
        });
        Ok(Self {
            shared: shared.clone(),
            id,
            provider,
            provider_name,
            model,
            price,
            system,
            tools,
            approvals: SessionApprovals::default(),
            messages,
            usage,
            audit,
            history_dir,
            created,
            profile: launch.profile.clone(),
            workspace,
        })
    }

    fn save(&self) {
        let Some(dir) = &self.history_dir else { return };
        let saved = Saved {
            version: 1,
            id: self.id.clone(),
            profile: self.profile.clone(),
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            workspace: self.workspace.clone(),
            created: self.created.clone(),
            updated: jiff::Timestamp::now().to_string(),
            messages: self.messages.clone(),
            usage: self.usage,
            audit_head: self.audit.as_ref().map(|a| a.head().to_owned()),
        };
        if let Err(e) = saved.save(dir) {
            tracing::warn!("cannot save the conversation: {e}");
        }
    }

    fn audit(&mut self, event: Event) {
        if let Some(a) = &mut self.audit
            && let Err(e) = a.append(event)
        {
            tracing::warn!("cannot write the audit log: {e}");
            self.shared
                .error(format!("cannot write the audit log: {e}"));
            self.audit = None;
        }
    }

    /// One user message and everything it leads to.
    async fn turn(
        &mut self,
        text: String,
        rx: &mut mpsc::UnboundedReceiver<Input>,
    ) -> Result<(), Interrupt> {
        let text = self.tools.redactor.text_known(&text);
        self.shared
            .update(|v| v.items.push(Item::User(text.clone())));
        // After a cancelled reply the last message is the user's: add to it.
        match self.messages.last_mut() {
            Some(m) if m.role == Role::User => m.content.push(Block::Text { text }),
            _ => self.messages.push(Message::user_text(text)),
        }
        self.save();
        for _ in 0..MAX_ROUNDS {
            let reply = self.ask_model(rx).await?;
            let Some(reply) = reply else {
                return Ok(());
            };
            let calls: Vec<(String, String, serde_json::Value)> = reply
                .message
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();
            self.messages.push(reply.message.clone());
            self.save();
            match &reply.stop {
                StopReason::Refusal(why) => self.shared.notice(format!(
                    "the model declined{}",
                    why.as_ref().map(|w| format!(" ({w})")).unwrap_or_default()
                )),
                StopReason::MaxTokens => self.shared.notice("the reply hit its token limit"),
                StopReason::Other(o) => tracing::debug!("stop reason {o}"),
                StopReason::EndTurn | StopReason::ToolUse => {}
            }
            if calls.is_empty() {
                return Ok(());
            }
            let truncated = matches!(reply.stop, StopReason::MaxTokens);
            let mut results = Vec::new();
            let mut interrupted = None;
            for (id, name, input) in calls {
                if interrupted.is_some() {
                    results.push(tool_result(
                        &id,
                        Output {
                            text: "cancelled by the user".into(),
                            is_error: true,
                        },
                    ));
                    continue;
                }
                let bad = reply
                    .bad_inputs
                    .iter()
                    .find(|(b, _)| *b == id)
                    .map(|(_, e)| e.clone());
                let out = if let Some(e) = bad {
                    let why = if truncated {
                        "the reply was cut off by its token limit"
                    } else {
                        "it was not valid JSON"
                    };
                    self.record_refused(&id, &name, &input, "invalid", why);
                    Output {
                        text: format!("the tool input could not be used: {why} ({e})"),
                        is_error: true,
                    }
                } else {
                    match self.call_tool(&id, &name, &input, rx).await {
                        Ok(out) => out,
                        Err(i) => {
                            interrupted = Some(i);
                            Output {
                                text: "cancelled by the user".into(),
                                is_error: true,
                            }
                        }
                    }
                };
                results.push(tool_result(&id, out));
            }
            self.messages.push(Message {
                role: Role::User,
                content: results,
            });
            self.save();
            if let Some(i) = interrupted {
                return Err(i);
            }
        }
        self.shared.notice(format!(
            "stopped after {MAX_ROUNDS} rounds of tool calls: send a message to continue"
        ));
        Ok(())
    }

    /// Streams the model's reply, retrying what can be retried. `None`:
    /// it failed and the user was told.
    async fn ask_model(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<Input>,
    ) -> Result<Option<Reply>, Interrupt> {
        let req = ChatRequest {
            model: self.model.clone(),
            system: self.system.clone(),
            messages: Arc::new(self.messages.clone()),
            tools: Arc::new(self.tools.specs()),
            max_tokens: None,
        };
        let mut wait = Duration::from_secs(2);
        for attempt in 1..=TRIES {
            self.shared.update(|v| v.status = "thinking…".into());
            let shared = self.shared.clone();
            let mut streamed = false;
            let mut on_delta = |d: Delta| {
                streamed = true;
                shared.update(|v| match d {
                    Delta::Text(t) => v.streaming_text(false).push_str(&t),
                    Delta::Thinking(t) => v.streaming_text(true).push_str(&t),
                    Delta::ToolStart { name } => {
                        v.close_streams();
                        v.status = format!("preparing {name}…");
                    }
                });
            };
            let result = tokio::select! {
                r = self.provider.chat(&req, &mut on_delta) => r,
                i = next_interrupt(rx) => {
                    self.shared.update(View::close_streams);
                    return Err(i);
                }
            };
            self.shared.update(View::close_streams);
            match result {
                Ok(reply) => {
                    self.usage.add(reply.usage);
                    let (usage, cost) = (self.usage, self.price.map(|p| self.usage.cost(&p)));
                    self.shared.update(|v| {
                        v.usage = usage;
                        v.cost = cost;
                    });
                    return Ok(Some(reply));
                }
                Err(e) if e.retryable() && !streamed && attempt < TRIES => {
                    self.retry_pause(&e, wait, rx).await?;
                    wait *= 2;
                }
                Err(e) => {
                    self.shared.error(describe_error(&e, &self.provider_name));
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    async fn retry_pause(
        &self,
        e: &ProviderError,
        wait: Duration,
        rx: &mut mpsc::UnboundedReceiver<Input>,
    ) -> Result<(), Interrupt> {
        self.shared
            .update(|v| v.status = format!("{e}; trying again in {}s…", wait.as_secs()));
        tokio::select! {
            () = tokio::time::sleep(wait) => Ok(()),
            i = next_interrupt(rx) => Err(i),
        }
    }

    fn record_refused(
        &mut self,
        id: &str,
        tool: &str,
        input: &serde_json::Value,
        decision: &str,
        why: &str,
    ) {
        let args = self.tools.redactor.value(input);
        self.audit(Event::Tool {
            id: id.to_owned(),
            tool: tool.to_owned(),
            args,
            decision: decision.to_owned(),
            reason: why.to_owned(),
            result: None,
            ok: None,
        });
        let (id, tool, why) = (id.to_owned(), tool.to_owned(), why.to_owned());
        self.shared.update(|v| {
            v.items.push(Item::Tool(Box::new(ToolItem {
                id,
                tool: tool.clone(),
                summary: tool,
                state: ToolState::Denied(why),
                result: None,
                term: None,
            })));
        });
    }

    /// Prepares, decides, asks, runs and records one call.
    async fn call_tool(
        &mut self,
        id: &str,
        name: &str,
        input: &serde_json::Value,
        rx: &mut mpsc::UnboundedReceiver<Input>,
    ) -> Result<Output, Interrupt> {
        let call = match self.tools.prepare(id, name, input) {
            Ok(c) => c,
            Err(e) => {
                self.record_refused(id, name, input, "invalid", &e);
                return Ok(Output {
                    text: e,
                    is_error: true,
                });
            }
        };
        let summary = summarize(&call, &self.tools);
        self.shared.update(|v| {
            v.items.push(Item::Tool(Box::new(ToolItem {
                id: id.to_owned(),
                tool: name.to_owned(),
                summary: summary.clone(),
                state: ToolState::Asking,
                result: None,
                term: None,
            })));
        });
        if self.audit.is_none() {
            let why = "TermOS cannot write the audit log, so no tool runs";
            self.set_tool_state(id, ToolState::Denied(why.into()));
            return Ok(Output {
                text: why.into(),
                is_error: true,
            });
        }
        let (decision, reason) = match self.tools.decide(&call, &self.approvals) {
            Decision::Allow(why) => ("allowed", why),
            Decision::Deny(why) => {
                self.finish_call(&call, "denied", &why, None);
                self.set_tool_state(id, ToolState::Denied(why.clone()));
                return Ok(Output {
                    text: format!("refused by the permission profile: {why}"),
                    is_error: true,
                });
            }
            Decision::Ask { why, remember } => {
                let pending = Pending {
                    call_id: id.to_owned(),
                    tool: name.to_owned(),
                    summary: summary.clone(),
                    why: why.clone(),
                    remember,
                    preview: call.preview.clone(),
                };
                self.shared
                    .phase(Phase::Confirm(pending), "waiting for your answer");
                let answer = loop {
                    match rx.recv().await {
                        Some(Input::Approve { remember: r }) => break Some(r && remember),
                        Some(Input::Deny) => break None,
                        Some(Input::Cancel) => {
                            self.finish_call(&call, "cancelled", "the user cancelled", None);
                            self.set_tool_state(id, ToolState::Cancelled);
                            self.shared.phase(Phase::Busy, "");
                            return Err(Interrupt::Cancelled);
                        }
                        Some(Input::Close) | None => return Err(Interrupt::Closed),
                        Some(_) => {}
                    }
                };
                self.shared.phase(Phase::Busy, "");
                match answer {
                    Some(remember) => {
                        if remember {
                            for a in &call.actions {
                                self.approvals.approve(a);
                            }
                        }
                        (
                            "approved",
                            if remember {
                                format!("{why}; approved for this session")
                            } else {
                                why
                            },
                        )
                    }
                    None => {
                        self.finish_call(&call, "refused", "the user refused", None);
                        self.set_tool_state(id, ToolState::Denied("you refused".into()));
                        return Ok(Output {
                            text: "the user refused this call; ask them how to go on".into(),
                            is_error: true,
                        });
                    }
                }
            }
        };
        self.set_tool_state(id, ToolState::Running);
        self.shared
            .update(|v| v.status = format!("running {name}…"));
        let shared = self.shared.clone();
        let tid = id.to_owned();
        let mut on_output = |bytes: &[u8]| {
            shared.update(|v| {
                if let Some(t) = v.tool_mut(&tid) {
                    let term = t
                        .term
                        .get_or_insert_with(|| AlacrittyVt::new(TERM_COLS, TERM_ROWS, 2000));
                    term.advance(bytes);
                }
            });
        };
        let out = tokio::select! {
            out = self.tools.run(&call, &mut on_output) => out,
            i = next_interrupt(rx) => {
                self.finish_call(&call, decision, &reason, Some(&Output { text: "cancelled by the user".into(), is_error: true }));
                self.set_tool_state(id, ToolState::Cancelled);
                return Err(i);
            }
        };
        let out = Output {
            text: self.tools.redactor.text(&out.text),
            is_error: out.is_error,
        };
        self.finish_call(&call, decision, &reason, Some(&out));
        let first_lines: String = out.text.lines().take(6).collect::<Vec<_>>().join("\n");
        let state = if out.is_error {
            ToolState::Failed
        } else {
            ToolState::Done
        };
        self.shared.update(|v| {
            if let Some(t) = v.tool_mut(id) {
                t.state = state;
                if t.term.is_none() {
                    t.result = Some(first_lines);
                } else {
                    t.result = out.text.lines().next().map(str::to_owned);
                }
            }
        });
        Ok(out)
    }

    fn set_tool_state(&self, id: &str, state: ToolState) {
        self.shared.update(|v| {
            if let Some(t) = v.tool_mut(id) {
                t.state = state;
            }
        });
    }

    fn finish_call(&mut self, call: &Call, decision: &str, reason: &str, out: Option<&Output>) {
        let args = self.tools.redactor.value(&call.args);
        let result = out.map(|o| {
            let first: String = o.text.lines().take(3).collect::<Vec<_>>().join(" | ");
            let first: String = first.chars().take(300).collect();
            self.tools.redactor.text(&first)
        });
        self.audit(Event::Tool {
            id: call.id.clone(),
            tool: call.tool.clone(),
            args,
            decision: decision.to_owned(),
            reason: reason.to_owned(),
            result,
            ok: out.map(|o| !o.is_error),
        });
    }

    async fn end(mut self) {
        self.audit(Event::Note {
            text: "the session window closed".into(),
        });
        self.save();
        let hub = std::mem::take(&mut self.tools.mcp);
        hub.shutdown().await;
    }
}

/// Waits for Cancel or Close (other input is ignored while busy).
async fn next_interrupt(rx: &mut mpsc::UnboundedReceiver<Input>) -> Interrupt {
    loop {
        match rx.recv().await {
            Some(Input::Cancel) => return Interrupt::Cancelled,
            Some(Input::Close) | None => return Interrupt::Closed,
            Some(_) => {}
        }
    }
}

/// Reads a secret, asking for the secrets file's passphrase while it is
/// locked.
async fn read_secret(
    slot: &Arc<SecretSlot>,
    name: &str,
    shared: &Shared,
    rx: &mut mpsc::UnboundedReceiver<Input>,
) -> Result<SecretString, Option<String>> {
    let mut error = None;
    loop {
        let (slot2, name2) = (Arc::clone(slot), name.to_owned());
        // Keyring calls may block on D-Bus.
        let got = tokio::task::spawn_blocking(move || slot2.get().get(&name2))
            .await
            .map_err(|e| Some(e.to_string()))?;
        match got {
            Ok(k) => return Ok(k),
            Err(SecretError::Locked) => {
                shared.phase(
                    Phase::NeedPassphrase {
                        error: error.take(),
                    },
                    "the secrets file is locked",
                );
                let pass = wait_for(rx, |i| match i {
                    Input::Passphrase(p) => Some(p),
                    _ => None,
                })
                .await
                .ok_or(None)?;
                shared.phase(Phase::Starting, "unlocking…");
                let slot2 = Arc::clone(slot);
                let unlocked = tokio::task::spawn_blocking(move || slot2.get().unlock(pass))
                    .await
                    .map_err(|e| Some(e.to_string()))?;
                if let Err(e) = unlocked {
                    error = Some(e.to_string());
                }
            }
            Err(e) => return Err(Some(e.to_string())),
        }
    }
}

/// What the sandbox hides: credentials in the home folder, the runtime
/// folder (D-Bus, ssh-agent and TermOS sockets), TermOS's saved
/// conversations and audit logs, and the secrets file.
fn hidden_paths(launch: &Launch, secrets_file: &std::path::Path) -> Vec<PathBuf> {
    let mut hidden: Vec<PathBuf> = launch
        .home
        .iter()
        .flat_map(|h| {
            crate::policy::SENSITIVE_IN_HOME
                .iter()
                .map(move |s| h.join(s))
        })
        .collect();
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        hidden.push(PathBuf::from(run));
    }
    hidden.push(launch.paths.runtime_dir.clone());
    hidden.push(history::dir(&launch.paths.state_dir));
    hidden.push(audit::dir(&launch.paths.state_dir));
    hidden.push(secrets_file.to_owned());
    hidden
}

fn tool_result(id: &str, out: Output) -> Block {
    Block::ToolResult {
        tool_use_id: id.to_owned(),
        content: out.text,
        is_error: out.is_error,
    }
}

/// One line about a call, for the transcript and the confirmation.
fn summarize(call: &Call, tools: &Toolbox) -> String {
    let arg = |k: &str| {
        call.args
            .get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let show = |k: &str| {
        let a = arg(k);
        if a.is_empty() {
            ".".to_owned()
        } else {
            tools.show(&tools.policy.resolve(a))
        }
    };
    match call.tool.as_str() {
        "read_file" => format!("read {}", show("path")),
        "list_dir" => format!("list {}", show("path")),
        "search_code" => format!("search {:?} in {}", arg("pattern"), show("path")),
        "write_file" => format!("write {}", show("path")),
        "edit_file" => format!("edit {}", show("path")),
        "run_command" => {
            let net = call
                .args
                .get("network")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            format!(
                "$ {}{}",
                tools.redactor.text(arg("command")),
                if net { "  (network)" } else { "" }
            )
        }
        "fetch_url" => format!("fetch {}", arg("url")),
        other => match &call.preview {
            Preview::Mcp { server, tool, .. } => format!("MCP {server}: {tool}"),
            _ => other.to_owned(),
        },
    }
}

fn describe_error(e: &ProviderError, provider: &str) -> String {
    use crate::provider::ErrorKind;
    match e.kind {
        ErrorKind::Auth => format!(
            "{provider} refused the API key ({e}); store the right one with `termos secret set <name>`"
        ),
        _ => format!("{provider}: {e}"),
    }
}

/// Transcript items for a stored message (when resuming).
fn push_message_items(v: &mut View, m: &Message) {
    for b in &m.content {
        match (m.role, b) {
            (Role::User, Block::Text { text }) => v.items.push(Item::User(text.clone())),
            (Role::Assistant, Block::Text { text }) => v.items.push(Item::Assistant {
                text: text.clone(),
                done: true,
            }),
            (Role::Assistant, Block::ToolUse { id, name, .. }) => {
                v.items.push(Item::Tool(Box::new(ToolItem {
                    id: id.clone(),
                    tool: name.clone(),
                    summary: name.clone(),
                    state: ToolState::Done,
                    result: None,
                    term: None,
                })))
            }
            _ => {}
        }
    }
}
