//! The agents aiterm knows how to work with.
//!
//! aiterm was built around Claude Code, and the assumption is spread thin
//! across the codebase rather than concentrated: the session store path, the
//! launch flags, the roster command and the transcript format are all just
//! *there*, in whatever function needed them. That is fine for one agent and
//! becomes untenable at two, because the second one does not announce itself —
//! it shows up as a session list that silently omits half your work, or a
//! search index that only covers one tool.
//!
//! This module is the one place that knows an agent exists. A backend answers
//! three questions, and they are deliberately separate:
//!
//! - **Who are you?** `id` and `display_name`. `id` is written onto every
//!   session the backend yields and is what the UI switches its icon on.
//! - **Are you here?** `detect`, which is cheap enough to call whenever the
//!   answer is wanted and never assumes a tool is installed.
//! - **Where are your sessions?** `sessions`, a [`SessionProvider`].
//!
//! ## What is *not* here yet, and why that matters
//!
//! Listing, indexing and transcript lookup route through this registry. A great
//! deal does not, and a second backend will find every one of these still
//! hard-wired to Claude Code:
//!
//! - **Liveness.** `read_roster` shells out to `claude agents --json`.
//! - **Lifecycle.** Resume, fork, stop and the `--session-id` mint in the UI
//!   all speak Claude Code's flags.
//! - **Panels.** Tasks, artifacts, agents and the model pills parse Claude
//!   Code's transcript records and read `~/.claude`.
//! - **Trash.** `session_delete` and restore know `~/.claude/projects` layout.
//!
//! Each of those is a real decision rather than a mechanical port — a CLI agent
//! has to be *asked* whether a session is alive, while an API-backed one knows
//! for free — so they are left explicit rather than hidden behind a trait that
//! pretends to abstract them. Adding a backend today gets you rows in the list
//! and hits in search. Everything else is still to do, and this comment is the
//! honest list of what.

use std::time::Duration;

use serde::Serialize;

use crate::sessions::{ClaudeProvider, Session, SessionProvider};

/// What an engine supports, so the UI can stop asking what it is called.
///
/// Every flag names one affordance the renderer used to gate on an agent id —
/// `s.agent === "api"` and `s.agent === "claude"` scattered across the sidebar
/// and the composer. A backend declares its own; an id with no backend at all
/// (an index row from an engine since removed) gets all false, which shows a
/// row with no buttons rather than a row offering claude's.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Caps {
    /// ⑂ in the sidebar — branch this session into a new one.
    pub fork: bool,
    /// ✦ re-key — start fresh under an id aiterm already knows.
    pub clear: bool,
    /// ▶ — reopen this session where it left off.
    pub resume: bool,
    /// The 250 ms screen poll in `term/screen.ts` and the Tui* dialogs that
    /// drive the agent's own TUI by typing into it.
    ///
    /// Separate from `panels` because they read different things: this one
    /// parses what is on the screen, `panels` reads a transcript off disk. An
    /// engine can have either without the other — today both run against every
    /// tab regardless of what is in it — so one combined flag would force a
    /// backend to claim capabilities it does not have to get the one it does.
    pub tui_drive: bool,
    /// Transcript panels — tasks, artifacts, agents — and the composer pills
    /// that send `/model`, `/effort` and `/rewind`.
    pub panels: bool,
    /// The Agent panel's Tasks and Artifacts tabs — this engine records a task
    /// list and file edits in a shape aiterm knows how to read. Separate from
    /// `panels`: that flag also turns on the `/model` `/effort` `/rewind`
    /// pills, which speak claude's slash commands — an engine can be readable
    /// here without being steerable there.
    pub tasks: bool,
    /// 🗑 — move this session to `~/.claude/trash`.
    ///
    /// Off by default, and that direction matters more here than anywhere else
    /// in this struct. For most engines `session_delete` renames whatever
    /// `find_session_file` hands it, and not every provider's answer is a file
    /// that belongs to one session — which is why the default is refusal, and
    /// why OpenCode (whose answer is `opencode.db`, the single database holding
    /// *every* OpenCode conversation on the machine) only claims this because
    /// `session_delete` routes its ids to a row-level delete instead of the
    /// rename. A backend gets a 🗑 by claiming it, never by omission, and
    /// `session_delete` checks this itself rather than trusting the UI to have
    /// hidden the button.
    pub delete: bool,
    /// The engine has configuration aiterm can read and show — the Settings
    /// button on its row in the Agents pane. Off by default: an engine whose
    /// config aiterm cannot read is better with no button than with an empty
    /// panel.
    pub config: bool,
    /// Something outside aiterm reports whether this engine's sessions are
    /// alive — for claude, `claude agents --json`, which `live_session_ids`
    /// reads.
    ///
    /// Only claude has one, and the flag exists so the sidebar can tell "not in
    /// the roster" from "not running". A claude row must trust the roster and
    /// nothing else: after a background-mode resume the tab holds the frozen
    /// parent while the conversation runs under another id, so an open tab is
    /// not evidence of life and treating it as such put the green dot on the
    /// wrong row. An engine with no roster has the opposite problem — nothing
    /// could ever report it, so a Codex session ran with no dot at all — and
    /// there a live tab in this window is the only evidence there is.
    pub roster_liveness: bool,
}

/// What is known about an agent on this machine right now.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Detection {
    pub id: String,
    pub display_name: String,
    /// Whether aiterm can actually use it. For a CLI agent this means the
    /// binary is on PATH; a future API-backed backend would report whether it
    /// has credentials.
    pub available: bool,
    /// First line of `<bin> --version`, when it answered. `None` covers both
    /// "not installed" and "installed but would not say", which are different
    /// facts — `available` is the one to branch on.
    pub version: Option<String>,
    /// Resolved binary path, so the UI can show *which* copy was found when
    /// several are installed.
    pub path: Option<String>,
    /// What this engine supports. Rides here because the frontend already
    /// fetches detection once on mount, and it covers absent backends too — a
    /// session row belonging to an uninstalled engine still has to resolve.
    pub caps: Caps,
}

/// A model a backend can be started on, and the effort levels it accepts.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ModelOption {
    /// What goes on the command line.
    pub id: String,
    pub display_name: String,
    /// Effort levels valid *for this model*. Empty when the agent has no such
    /// concept. Per-model rather than per-agent because they genuinely differ:
    /// Codex publishes a different set for each model.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

/// One way an engine can be told how much to ask before acting, spelled the
/// way that engine spells it. See `permissions.rs` for how one is chosen.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct PermissionMode {
    /// Stable id, the thing stored. Where the engine names the mode itself
    /// (claude's and grok's `--permission-mode` values) that name is used, so
    /// the file reads the way the engine's own docs do.
    pub id: &'static str,
    pub label: &'static str,
    /// One line on what it does, in the engine's own terms.
    pub note: &'static str,
    /// The flags it is spelled as, each a complete `--flag value` group.
    /// Empty for the engine's own default, which is "pass nothing".
    pub flags: &'static [&'static str],
}

/// What to start. Every field optional — the honest default for all of them is
/// "whatever the agent would do on its own", which is not the same as any value
/// we could pick for it.
#[derive(serde::Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// A session id aiterm has minted. Only meaningful where the agent accepts
    /// one — see `mints_session_id`.
    pub session_id: Option<String>,
    /// The id of an API provider from the provider store. Only meaningful to a
    /// backend that runs API models; everything else ignores it. Never the key
    /// itself — that is resolved at spawn time and never crosses a command
    /// line.
    pub provider: Option<String>,
    /// The first thing to say — typed on the home screen before the session
    /// existed. Each engine spells it its own way (a positional for claude,
    /// codex and grok; `--prompt` for opencode and the chat harness), so it
    /// goes on the command here rather than being typed into the pty later,
    /// where it would race the engine's startup.
    pub prompt: Option<String>,
}

pub trait AgentBackend: Send + Sync {
    /// Stable identifier. Written onto every session this backend yields, so
    /// changing it orphans the `agent` field on rows already in the index.
    fn id(&self) -> &'static str;

    /// Human-facing name, for settings and empty states.
    fn display_name(&self) -> &'static str;

    /// Is this agent usable on this machine?
    ///
    /// Called on demand rather than polled: availability changes when someone
    /// installs something, which is not something to spend a timer on. The
    /// PATH lookup is pure filesystem; only reading a version spawns anything,
    /// and only when the binary was found.
    fn detect(&self) -> Detection;

    /// Where this backend's sessions live.
    fn sessions(&self) -> &dyn SessionProvider;

    /// Models this agent can be started on, best-effort. An empty list means
    /// "we do not know" — the UI offers the agent's own default rather than
    /// inventing names.
    fn models(&self) -> Vec<ModelOption> {
        Vec::new()
    }

    /// Whether `--session-id`-style pre-minting works.
    ///
    /// This is not a detail: aiterm mints the id before launching so a new tab
    /// has a sidebar row from the first frame. Where an agent will not take
    /// one, the id is a tab handle only, no panel should be keyed to it, and
    /// its placeholder row stays until the tab closes. Saying so here is what
    /// keeps the UI from pointing panels at a session that will never exist.
    fn mints_session_id(&self) -> bool {
        false
    }

    /// The shell command that starts a session.
    ///
    /// Built here rather than in the frontend, which is where it used to live
    /// as a hardcoded `CLAUDE_CMD` string. Command-line syntax is the one thing
    /// that is certainly per-agent, so it belongs with the agent — otherwise
    /// adding one means editing the renderer.
    fn launch(&self, spec: &LaunchSpec) -> String;

    /// Whether the new-session menu offers this agent as a source of its own.
    /// An engine that is reached some other way — OpenCode runs whatever the
    /// Model-access dropdown picks — says no, so the menu doesn't grow a
    /// second door to the same room.
    fn offered(&self) -> bool {
        true
    }

    /// What this engine supports, for the UI to gate on instead of the id.
    /// Defaulting to all-false is the safe direction: a backend that has not
    /// said it can be forked offers no ⑂ rather than one that fails.
    fn caps(&self) -> Caps {
        Caps::default()
    }

    /// The permission modes this engine can be opened in, the first being what
    /// aiterm uses when nothing is stored. Empty where the engine has no such
    /// switch — the API chat harness — and the setting then does not show.
    /// Applied by the launch resolver to every start, resume and clear, never
    /// by `launch` itself: see `permissions.rs`.
    fn permission_modes(&self) -> &'static [PermissionMode] {
        &[]
    }

    /// The command that reopens an existing session. `None` where the engine
    /// cannot be told to resume — the caller keeps whatever fallback it had.
    fn resume(&self, session_id: &str) -> Option<String> {
        let _ = session_id;
        None
    }

    /// The command that starts a fresh session under an id aiterm already
    /// knows. `None` where the engine will not take an id up front.
    fn clear(&self, session_id: &str) -> Option<String> {
        let _ = session_id;
        None
    }

    /// What an existing session was running on — `(providerID, modelID)` in
    /// the engine's own spelling — so a reopen can put its environment back.
    /// `None` for an engine whose sessions carry no API context, which is
    /// every engine that authenticates itself.
    fn api_resume_context(&self, session_id: &str) -> Option<(String, String)> {
        let _ = session_id;
        None
    }

    /// Whether this engine will run a model from this API provider.
    ///
    /// The routing question that used to be an `&&` in the renderer. Each
    /// backend answers about itself and the resolver takes the first willing
    /// one, so a new engine inserts itself by answering rather than by an edit
    /// at the call site.
    fn accepts_api(&self, provider: &crate::providers::Provider) -> bool {
        let _ = provider;
        false
    }

    /// How this engine spells a provider's model id on its command line.
    /// OpenCode wants its own catalog prefix — which one depends on WHICH
    /// provider, so the provider rides along; a raw id is right for everyone
    /// else. The engine's spelling is the engine's business — the resolver
    /// must not learn it.
    fn api_model_slug(&self, provider: &crate::providers::Provider, model_id: &str) -> String {
        let _ = provider;
        model_id.to_string()
    }

    /// Whether a provider-backed launch needs the provider's key in its
    /// process environment.
    ///
    /// False by default, deliberately: an engine gets a secret only by asking
    /// for it. `ChatBackend` reads the provider store itself and must never be
    /// handed a key; an external CLI like OpenCode has no other way to get one.
    fn needs_provider_key_env(&self) -> bool {
        false
    }
}

/// Shell-quote a value going onto a command line.
///
/// Model ids and effort levels come from the frontend. They are chosen from
/// lists we produced, but "the UI only sends good values" is not a security
/// boundary — this string is handed to `$SHELL -ic`.
/// The prompt worth passing, if any: a blank one is no prompt, not an empty
/// argument the engine would have to make sense of.
pub(crate) fn prompt_of(spec: &LaunchSpec) -> Option<&str> {
    spec.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

pub(crate) fn q(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Claude's permission modes, from `claude --help` 2.1.220's `--permission-mode`
/// choices, plus `--dangerously-skip-permissions` for the one that never asks.
///
/// The first is the flag pair every claude launch carried before this was a
/// setting (the frontend's old `CLAUDE_CMD`, relocated), so a fresh install
/// behaves as it always did. `--allow-dangerously-skip-permissions` rides
/// along with every asking mode: it does not skip anything itself, it makes
/// "bypass permissions" reachable from claude's own shift+tab cycle, which the
/// composer's mode pill relies on.
pub const CLAUDE_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode {
        id: "auto",
        label: "Auto",
        note: "Claude's auto mode: routine actions run, the rest ask. What aiterm has always started with.",
        flags: &["--permission-mode auto", "--allow-dangerously-skip-permissions"],
    },
    PermissionMode {
        id: "manual",
        label: "Ask for everything",
        note: "Every tool call waits for approval.",
        flags: &["--permission-mode manual", "--allow-dangerously-skip-permissions"],
    },
    PermissionMode {
        id: "acceptEdits",
        label: "Accept edits",
        note: "File edits run without asking; commands still ask.",
        flags: &["--permission-mode acceptEdits", "--allow-dangerously-skip-permissions"],
    },
    PermissionMode {
        id: "plan",
        label: "Plan mode",
        note: "Reads and plans, changes nothing until told to.",
        flags: &["--permission-mode plan", "--allow-dangerously-skip-permissions"],
    },
    PermissionMode {
        id: "bypassPermissions",
        label: "Skip all permissions",
        note: "--dangerously-skip-permissions: nothing asks. For work you would let run unattended.",
        flags: &["--dangerously-skip-permissions"],
    },
];

/// Codex's, from `codex --help`: an approval policy (`-a`) and a sandbox
/// (`-s`), or the one flag that switches both off.
pub const CODEX_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode {
        id: "default",
        label: "Codex's own default",
        note: "Whatever ~/.codex/config.toml says; out of the box, the model asks before commands.",
        flags: &[],
    },
    PermissionMode {
        id: "never",
        label: "Never ask, workspace sandbox",
        note: "-a never -s workspace-write: commands run without approval, confined to the workspace.",
        flags: &["-a never", "-s workspace-write"],
    },
    PermissionMode {
        id: "bypass",
        label: "Skip approvals and sandbox (yolo)",
        note: "--dangerously-bypass-approvals-and-sandbox — what `--yolo` used to spell (the alias is gone from 0.150.1's binary): nothing asks and nothing is confined.",
        flags: &["--dangerously-bypass-approvals-and-sandbox"],
    },
];

/// OpenCode's: permissions live in opencode.json, and the CLI's one switch is
/// `--auto`, which approves anything that file does not explicitly deny.
pub const OPENCODE_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode {
        id: "default",
        label: "Ask per opencode.json",
        note: "OpenCode's own permission config decides what asks.",
        flags: &[],
    },
    PermissionMode {
        id: "auto",
        label: "Auto-approve",
        note: "--auto: approves everything opencode.json does not explicitly deny.",
        flags: &["--auto"],
    },
];

/// OpenCode's own provider declarations, parsed from config text: id, its
/// `options.baseURL`, and the env var its `options.apiKey` reads when spelled
/// `{env:NAME}`. The shape is opencode.json's:
/// `provider.<id>.options.{baseURL, apiKey}`. [observed: opencode.json on
/// this machine, 2026-08-31 — provider "local", baseURL
/// "http://127.0.0.1:8099/v1", apiKey "{env:LOCAL_LLM_API_KEY}"]
pub(crate) fn parse_opencode_providers(text: &str) -> Vec<(String, String, Option<String>)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(map) = v.get("provider").and_then(|p| p.as_object()) else { return Vec::new() };
    map.iter()
        .filter_map(|(id, cfg)| {
            let base = cfg.pointer("/options/baseURL")?.as_str()?.to_string();
            let env = cfg
                .pointer("/options/apiKey")
                .and_then(|k| k.as_str())
                .and_then(|k| k.strip_prefix("{env:"))
                .and_then(|k| k.strip_suffix('}'))
                .map(str::to_string);
            Some((id.clone(), base, env))
        })
        .collect()
}

/// `parse_opencode_providers` over `~/.config/opencode/opencode.json` —
/// read on every ask, which is launch-time only, so a config edit between
/// launches is always seen. Missing or unparseable file: no declarations.
fn opencode_declared_providers() -> Vec<(String, String, Option<String>)> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    match std::fs::read_to_string(home.join(".config/opencode/opencode.json")) {
        Ok(text) => parse_opencode_providers(&text),
        Err(_) => Vec::new(),
    }
}

/// Two spellings of the same endpoint. Deliberately narrow: lowercase,
/// trailing slashes trimmed, and loopback's two names folded together —
/// this machine's opencode.json says 127.0.0.1 where the provider store
/// says localhost, and that one mismatch is the difference between OpenCode
/// running a local model and the chat console taking it.
fn same_base_url(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .replace("//127.0.0.1", "//localhost")
    };
    norm(a) == norm(b)
}

/// The opencode.json provider serving the same endpoint as an aiterm
/// provider: its id (OpenCode's catalog prefix for the launch) and the env
/// var its key rides in. The bridge that lets OpenCode run a provider it
/// already knows — the local llama router — rather than only OpenRouter.
pub(crate) fn opencode_provider_matching(
    p: &crate::providers::Provider,
) -> Option<(String, Option<String>)> {
    opencode_declared_providers()
        .into_iter()
        .find(|(_, base, _)| same_base_url(base, &p.base_url))
        .map(|(id, _, env)| (id, env))
}

pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    fn detect(&self) -> Detection {
        Detection { caps: self.caps(), ..detect_cli(self.id(), self.display_name(), "claude") }
    }
    fn sessions(&self) -> &dyn SessionProvider {
        &ClaudeProvider
    }

    fn mints_session_id(&self) -> bool {
        true
    }

    /// Everything. The subsystems the flags gate were all written against
    /// Claude Code and only work today because it is the engine that runs them.
    fn caps(&self) -> Caps {
        Caps {
            fork: true,
            clear: true,
            resume: true,
            tui_drive: true,
            panels: true,
            tasks: true,
            delete: true,
            config: true,
            roster_liveness: true,
        }
    }

    /// The ordinary launch with `--resume <id>` in place of the minted id.
    ///
    /// Built from `launch` rather than from a second copy of the flags: the
    /// permission mode and the hook `--settings` must be the same on a resume
    /// as on a start, and two spellings of that would drift.
    ///
    /// The id is quoted, like every other value that reaches `$SHELL -ic`.
    /// Anything reading this command back to decide "is this tab reopening
    /// that conversation" — the re-key guard watches for exactly that — has to
    /// look for the quoted form.
    fn resume(&self, session_id: &str) -> Option<String> {
        Some(format!(
            "{} --resume {}",
            self.launch(&LaunchSpec::default()),
            q(session_id)
        ))
    }

    /// A `/clear` re-key: same command, new conversation, id aiterm chose.
    fn clear(&self, session_id: &str) -> Option<String> {
        Some(self.launch(&LaunchSpec {
            session_id: Some(session_id.to_string()),
            ..Default::default()
        }))
    }

    /// The aliases `claude --help` documents, plus the effort levels it lists.
    ///
    /// Hardcoded because Claude Code publishes no machine-readable list — the
    /// `/model` picker is drawn in the TUI and there is no cache file to read,
    /// unlike Codex. These come from `--help` on 2.1.220 and will age; the
    /// blank "agent default" option in the UI is the escape hatch, and picking
    /// nothing here is always safe.
    fn models(&self) -> Vec<ModelOption> {
        const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
        [("fable", "Fable"), ("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
            .into_iter()
            .map(|(id, name)| ModelOption {
                id: id.to_string(),
                display_name: name.to_string(),
                efforts: EFFORTS.iter().map(|s| s.to_string()).collect(),
                default_effort: None,
            })
            .collect()
    }

    fn permission_modes(&self) -> &'static [PermissionMode] {
        CLAUDE_PERMISSION_MODES
    }

    /// No permission flags here: they are the setting in `permissions.rs`,
    /// appended by the resolver so a resume carries the same ones as a start.
    /// Before that setting existed, `--permission-mode auto
    /// --allow-dangerously-skip-permissions` was hardcoded on this line.
    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut cmd = String::from("claude");
        // The SessionStart hook that reports the session id and pid back to
        // aiterm — additional settings, so the user's own config is untouched.
        // Absent only if the settings file could not be written, in which case
        // the launch works and the heuristics cover detection.
        if let Some(flag) = crate::hooklink::settings_flag() {
            cmd.push_str(&flag);
        }
        if let Some(m) = spec.model.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        if let Some(e) = spec.effort.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --effort {}", q(e)));
        }
        if let Some(id) = spec.session_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --session-id {}", q(id)));
        }
        if let Some(p) = prompt_of(spec) {
            cmd.push_str(&format!(" {}", q(p)));
        }
        cmd
    }
}

/// OpenAI Codex.
///
/// **Detection only.** Whether `codex` is installed is a fact aiterm can check
/// today, and worth showing — "Codex: not installed" tells you the difference
/// between a tool aiterm cannot use and one you have not set up. Its sessions
/// are another matter: the on-disk format has not been examined, so
/// [`CodexSessions`] finds nothing rather than guessing at a layout.
///
/// That split is deliberate. A provider that invented a plausible path would
/// fail by finding nothing in a way indistinguishable from "you have no Codex
/// sessions", and the first person to debug it would have to prove a negative.
/// This way the registry is honestly two backends, the settings panel can say
/// what is supported, and filling in the provider is a self-contained job with
/// nothing to unpick first.
pub struct CodexBackend;

/// Codex's session store: `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`.
///
/// Every rollout opens with a header line carrying the session id, the cwd it
/// was started in and the git branch, written the moment the session starts
/// rather than at the first prompt. That is the whole of what the sidebar
/// needs, and it means a Codex session is listable from the instant it exists
/// — unlike claude, whose transcript does not appear until you say something.
///
/// Only that first line is read. A rollout grows to whatever the conversation
/// grows to, and there is nothing further down that a session list wants.
pub struct CodexSessions;

/// What the header line of a rollout says about its session.
#[derive(Debug, PartialEq)]
struct CodexHeader {
    id: String,
    cwd: String,
    branch: Option<String>,
}

/// Read a rollout's header. `None` for anything that is not one — a partial
/// write, a file Codex has changed the shape of, or something else entirely
/// that happens to live under the same directory.
fn parse_codex_header(first_line: &str) -> Option<CodexHeader> {
    let v: serde_json::Value = serde_json::from_str(first_line).ok()?;
    let p = v.get("payload")?;
    // Codex 0.153.4: subagents share the root session_id but have their own
    // rollout id. They must not replace the user's transcript or timestamp.
    if codex_is_subagent(p) {
        return None;
    }
    Some(CodexHeader {
        id: p
            .get("id")
            .or_else(|| p.get("session_id"))?
            .as_str()?
            .to_string(),
        cwd: p.get("cwd")?.as_str()?.to_string(),
        branch: p
            .pointer("/git/branch")
            .and_then(|b| b.as_str())
            .map(String::from),
    })
}

fn codex_is_subagent(payload: &serde_json::Value) -> bool {
    payload
        .get("thread_source")
        .is_some_and(|source| source.as_str() != Some("user"))
        || payload
            .get("source")
            .is_some_and(|source| source.get("subagent").is_some())
}

fn codex_root() -> Option<std::path::PathBuf> {
    let dir = dirs::home_dir()?.join(".codex/sessions");
    let kind = std::fs::symlink_metadata(&dir).ok()?.file_type();
    (kind.is_dir() && !kind.is_symlink()).then_some(dir)
}

struct PinnedCodexRollout {
    file: std::fs::File,
    path: std::path::PathBuf,
}

/// Every `*.jsonl` under `dir`, recursively.
///
/// Codex files them by date, so the depth is fixed at three today — but a
/// walk costs the same as three nested reads and does not break the day that
/// changes.
fn codex_rollouts_bounded(
    dir: &crate::sessions::PinnedDiscoveryDirectory,
    depth: usize,
    budget: &mut crate::sessions::DiscoveryBudget,
    out: &mut Vec<PinnedCodexRollout>,
) {
    if budget.remaining() == 0 || !budget.enter_pinned_directory(dir.file(), depth) {
        return;
    }
    let Some(entries) = dir.entries() else {
        return;
    };
    for e in entries.flatten() {
        if budget.remaining() == 0 {
            break;
        }
        let name = e.file_name();
        if let Some(child) = dir.open_directory(&name) {
            codex_rollouts_bounded(&child, depth + 1, budget, out);
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            let Some(file) = dir.open_file(&name) else {
                continue;
            };
            if budget.claim_file() {
                out.push(PinnedCodexRollout {
                    file,
                    path: dir.child_path(&name),
                });
            }
        }
    }
}

fn pinned_codex_rollouts_bounded(
    root: &std::path::Path,
    budget: &mut crate::sessions::DiscoveryBudget,
) -> Vec<PinnedCodexRollout> {
    let Some(root) = crate::sessions::PinnedDiscoveryDirectory::open(root) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    codex_rollouts_bounded(&root, 0, budget, &mut output);
    output
}

/// The body of [`CodexSessions::scan_with_paths`], over an explicit root.
///
/// Split out so the parsing and skipping can be tested against a directory
/// built for the purpose. Through the trait it would read the real
/// `~/.codex/sessions`, which makes the test say different things on different
/// machines — and pass vacuously on any machine that has never run Codex.
fn scan_codex_dir(root: &std::path::Path) -> Vec<(Session, std::path::PathBuf)> {
    let mut budget = crate::sessions::DiscoveryBudget::new();
    scan_codex_dir_bounded(root, &mut budget)
}

fn scan_codex_dir_bounded(
    root: &std::path::Path,
    budget: &mut crate::sessions::DiscoveryBudget,
) -> Vec<(Session, std::path::PathBuf)> {
    let files = pinned_codex_rollouts_bounded(root, budget);
    // Only user roots become rows. Multiple legacy rollouts of the same root
    // still collapse by identity, keeping the most recently modified root.
    let mut by_session: std::collections::HashMap<String, (Session, std::path::PathBuf)> =
        std::collections::HashMap::new();
    for rollout in files {
        let Some((session, path)) = read_codex_row_from(rollout.file, &rollout.path) else {
            continue;
        };
        match by_session.get(&session.id) {
            Some((kept, _)) if kept.last_active >= session.last_active => {}
            _ => {
                by_session.insert(session.id.clone(), (session, path));
            }
        }
    }
    by_session.into_values().collect()
}

/// Every rollout file that belongs to `session_id`, oldest first.
///
/// Include explicitly marked subagents sharing the root's session_id for
/// family operations (trash, tasks and artifacts), without letting their
/// timestamps or transcript paths replace the root's discovery row.
pub fn codex_session_files(session_id: &str) -> Vec<std::path::PathBuf> {
    codex_root().map(|r| codex_session_files_in(&r, session_id)).unwrap_or_default()
}

/// The body of [`codex_session_files`], over an explicit root so it can be
/// tested against a directory built for the purpose.
fn codex_session_files_in(root: &std::path::Path, session_id: &str) -> Vec<std::path::PathBuf> {
    let mut budget = crate::sessions::DiscoveryBudget::new();
    let files = pinned_codex_rollouts_bounded(root, &mut budget);
    let mut mine: Vec<(u64, std::path::PathBuf)> = files
        .into_iter()
        .filter_map(|rollout| {
            let metadata = rollout.file.metadata().ok()?;
            let mut first = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(rollout.file), &mut first)
                .ok()?;
            let header: serde_json::Value = serde_json::from_str(&first).ok()?;
            let payload = header.get("payload")?;
            payload.get("cwd")?.as_str()?;
            let owner = if codex_is_subagent(payload) {
                payload.get("session_id")?.as_str()?
            } else {
                payload
                    .get("id")
                    .or_else(|| payload.get("session_id"))?
                    .as_str()?
            };
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            (owner == session_id).then_some((modified, rollout.path))
        })
        .collect();
    mine.sort_by_key(|(at, _)| *at);
    mine.into_iter().map(|(_, p)| p).collect()
}

/// One rollout file → its session row, or `None` if it is not a readable
/// rollout. The unit both [`scan_codex_dir`] and [`CodexSessions::find_session_file`]
/// build on, so the row a click selects opens its root transcript.
fn read_codex_row_from(
    file: std::fs::File,
    path: &std::path::Path,
) -> Option<(Session, std::path::PathBuf)> {
    let metadata = file.metadata().ok()?;
    let mut first = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut first).ok()?;
    let h = parse_codex_header(&first)?;
    let last_active = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((
        Session {
            id: h.id,
            // Stamped by the registry; see `scan_backends`.
            agent: String::new(),
            // The directory, matching how claude's rows read. A rollout has no
            // title of its own and inventing one from the first message would
            // mean reading the whole file for every row in the list.
            title: std::path::Path::new(&h.cwd)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| h.cwd.clone()),
            project_path: h.cwd.clone(),
            group_path: h.cwd,
            branch: h.branch,
            forked: false,
            background: false,
            fork_parent: None,
            last_active,
        },
        path.to_path_buf(),
    ))
}

/// Index just past `key:` / `"key":` in `s`, or `None`. Tolerant of both
/// spellings because codex plans exist on disk in both — quoted when the
/// arguments were serialized, bare when the model typed object-literal
/// shorthand into the exec runtime.
fn after_key(s: &str, key: &str) -> Option<usize> {
    for probe in [format!("\"{key}\""), key.to_string()] {
        let mut from = 0;
        while let Some(i) = s[from..].find(&probe) {
            let at = from + i + probe.len();
            let tail = s[at..].trim_start();
            if let Some(rest) = tail.strip_prefix(':') {
                return Some(s.len() - rest.len());
            }
            from = at;
        }
    }
    None
}

/// The slice inside the first balanced `open…close` pair, respecting
/// double-quoted strings — a plan step's own text may contain brackets.
fn balanced(s: &str, open: char, close: char) -> Option<&str> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let mut start = None;
    for (i, c) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        } else if c == open {
            depth += 1;
            if depth == 1 {
                start = Some(i + c.len_utf8());
            }
        } else if c == close {
            if depth == 1 {
                return Some(&s[start?..i]);
            }
            depth = depth.saturating_sub(1);
        }
    }
    None
}

/// Top-level `{…}` object slices inside array content, string-aware.
fn top_objects(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                depth += 1;
                if depth == 1 {
                    start = i + 1;
                }
            }
            '}' => {
                if depth == 1 {
                    out.push(&s[start..i]);
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    out
}

/// A double-quoted string at the (whitespace-trimmed) head of `s`, with
/// escapes resolved.
fn quoted_string_at(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut chars = s.chars();
    if chars.next() != Some('"') {
        return None;
    }
    let (mut out, mut esc) = (String::new(), false);
    for c in chars {
        if esc {
            out.push(match c {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

fn string_value(obj: &str, key: &str) -> Option<String> {
    quoted_string_at(&obj[after_key(obj, key)?..])
}

/// The `plan:[…]` steps out of JavaScript source calling
/// `tools.update_plan({…})` — what current codex CLIs actually write to the
/// rollout: the plan tool is reached through the exec JS runtime, so the
/// arguments are source text, not JSON. Returns `(step, status)` pairs.
pub fn extract_js_plan(input: &str) -> Option<Vec<(String, String)>> {
    let at = input.find("update_plan")?;
    let rest = &input[at + "update_plan".len()..];
    let arr = balanced(&rest[after_key(rest, "plan")?..], '[', ']')?;
    let steps: Vec<(String, String)> = top_objects(arr)
        .into_iter()
        .filter_map(|obj| {
            Some((
                string_value(obj, "step")?,
                string_value(obj, "status").unwrap_or_else(|| "pending".into()),
            ))
        })
        .collect();
    (!steps.is_empty()).then_some(steps)
}

/// File paths named by an `apply_patch` envelope embedded in an exec input.
/// The patch rides inside a JS string literal, so its newlines are the
/// two-character `\n` — a path ends at the first backslash, quote or real
/// newline, whichever comes first.
pub fn patch_paths(input: &str) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for (marker, tool) in [("*** Add File: ", "Write"), ("*** Update File: ", "Edit")] {
        let mut from = 0;
        while let Some(i) = input[from..].find(marker) {
            let start = from + i + marker.len();
            let end = input[start..]
                .find(|c: char| c == '\\' || c == '"' || c == '\n' || c == '\r')
                .map(|e| start + e)
                .unwrap_or(input.len());
            let path = input[start..end].trim();
            if !path.is_empty() {
                out.push((path.to_string(), tool));
            }
            from = end;
        }
    }
    out
}

fn plan_json_steps(v: &serde_json::Value) -> Option<Vec<(String, String)>> {
    let arr = v.get("plan")?.as_array()?;
    let steps: Vec<(String, String)> = arr
        .iter()
        .filter_map(|s| {
            Some((
                s.get("step")?.as_str()?.to_string(),
                s.get("status")
                    .and_then(|x| x.as_str())
                    .unwrap_or("pending")
                    .to_string(),
            ))
        })
        .collect();
    (!steps.is_empty()).then_some(steps)
}

/// The session's task list: its last `update_plan` call, since the tool's
/// semantics are replace-the-list. Both on-disk spellings are read — a
/// native `update_plan` call with JSON arguments, and the exec-embedded JS
/// form observed on codex 0.149.
pub fn codex_tasks(session_id: &str) -> Vec<crate::sessions::SessionTask> {
    use std::io::BufRead;
    let mut last: Option<Vec<(String, String)>> = None;
    for path in codex_session_files(session_id) {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("update_plan") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let Some(p) = v.get("payload") else { continue };
            // Call side only — outputs echo the same text back.
            let pt = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if pt != "custom_tool_call" && pt != "function_call" {
                continue;
            }
            let plan = if p.get("name").and_then(|n| n.as_str()) == Some("update_plan") {
                p.get("arguments")
                    .or_else(|| p.get("input"))
                    .and_then(|a| a.as_str())
                    .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
                    .as_ref()
                    .and_then(plan_json_steps)
            } else {
                p.get("input").and_then(|i| i.as_str()).and_then(extract_js_plan)
            };
            if plan.is_some() {
                last = plan;
            }
        }
    }
    last.unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, (step, status))| crate::sessions::SessionTask {
            id: (i + 1).to_string(),
            subject: step,
            status: match status.as_str() {
                "completed" | "in_progress" => status,
                _ => "pending".to_string(),
            },
            active_form: None,
            blocked_by: vec![],
        })
        .collect()
}

/// Files the session wrote — `patch_apply_end` events (approved patches,
/// which carry a structured `changes` map) and `apply_patch` envelopes
/// inside exec calls. Writes done through plain shell redirection are
/// invisible here; this is the subset codex records structurally.
pub fn codex_artifacts(session_id: &str) -> Vec<crate::sessions::Artifact> {
    use std::io::BufRead;
    let mut latest: std::collections::HashMap<String, (String, String)> = Default::default();
    for path in codex_session_files(session_id) {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("apply_patch") && !line.contains("patch_apply_end") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let Some(p) = v.get("payload") else { continue };
            match p.get("type").and_then(|t| t.as_str()) {
                Some("patch_apply_end") => {
                    if p.get("success").and_then(|b| b.as_bool()) == Some(false) {
                        continue;
                    }
                    let Some(changes) = p.get("changes").and_then(|c| c.as_object()) else {
                        continue;
                    };
                    for (fp, ch) in changes {
                        let tool = match ch.get("type").and_then(|t| t.as_str()) {
                            Some("add") => "Write",
                            Some("update") => "Edit",
                            _ => continue,
                        };
                        latest.insert(fp.clone(), (tool.to_string(), ts.clone()));
                    }
                }
                Some("custom_tool_call") | Some("function_call") => {
                    let Some(input) = p
                        .get("input")
                        .or_else(|| p.get("arguments"))
                        .and_then(|i| i.as_str())
                    else {
                        continue;
                    };
                    for (fp, tool) in patch_paths(input) {
                        latest.insert(fp, (tool.to_string(), ts.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    let mut out: Vec<crate::sessions::Artifact> = latest
        .into_iter()
        .map(|(path, (tool, at))| crate::sessions::Artifact { path, tool, at })
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

impl SessionProvider for CodexSessions {
    fn scan_with_paths(&self) -> Vec<(Session, std::path::PathBuf)> {
        codex_root().map(|r| scan_codex_dir(&r)).unwrap_or_default()
    }

    fn scan_with_paths_bounded(
        &self,
        budget: &mut crate::sessions::DiscoveryBudget,
    ) -> Vec<(Session, std::path::PathBuf)> {
        codex_root()
            .map(|root| scan_codex_dir_bounded(&root, budget))
            .unwrap_or_default()
    }

    fn find_session_file(&self, session_id: &str) -> Option<std::path::PathBuf> {
        // The id lives in the filename, but only for a conversation's ROOT
        // rollout — the per-turn files that share its session_id carry their own
        // ids in their names. So match on the header session_id (what a row is
        // keyed by) and return the newest rollout: the live file the row stands
        // for, and the one a delete or preview should act on.
        let root = codex_root()?;
        scan_codex_dir(&root)
            .into_iter()
            .find(|(s, _)| s.id == session_id)
            .map(|(_, p)| p)
    }

    fn tasks(&self, session_id: &str) -> Option<Vec<crate::sessions::SessionTask>> {
        Some(codex_tasks(session_id))
    }

    fn artifacts(&self, session_id: &str) -> Option<Vec<crate::sessions::Artifact>> {
        Some(codex_artifacts(session_id))
    }
}

impl AgentBackend for CodexBackend {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "Codex"
    }
    fn detect(&self) -> Detection {
        Detection { caps: self.caps(), ..detect_cli(self.id(), self.display_name(), "codex") }
    }
    fn sessions(&self) -> &dyn SessionProvider {
        &CodexSessions
    }

    /// Resume and delete.
    ///
    /// `resume` was parked while the reopen path was claude-shaped, but the
    /// `tui_drive` split settled that: a Codex row now resumes through the same
    /// clean branch `aiterm chat` uses — the id it is given is the id it
    /// reopens — so none of claude's stop-before-resume machinery (the part that
    /// once turned "resume this" into "lose this") sits on this route. And the
    /// row's id is the conversation's `session_id`, which is exactly what
    /// `codex resume <SESSION_ID>` resolves against — even though Codex now
    /// writes one rollout file per turn under that shared id (see
    /// `scan_codex_dir`).
    ///
    /// `delete` stays true, as it always was: a rollout is one file per session,
    /// so trashing one is exactly as scoped as trashing a claude transcript, and
    /// restore puts it back in Codex's own store. (The capability exists to keep
    /// OpenCode out, whose one shared database a per-row delete would have handed
    /// away wholesale.)
    fn caps(&self) -> Caps {
        Caps { resume: true, delete: true, tasks: true, ..Default::default() }
    }

    /// `codex resume <SESSION_ID>` — reopen a session by its UUID, confirmed on
    /// codex-cli (`Session id (UUID) or session name`). No model/effort override:
    /// resume reopens the session with the settings it already carries, unlike
    /// [`Self::launch`], which is spelling out a fresh start.
    fn resume(&self, session_id: &str) -> Option<String> {
        Some(format!("codex resume {}", q(session_id)))
    }

    /// Codex has no `--session-id`. `codex --help` offers `resume` and `fork`
    /// as subcommands and nothing that names a session up front, so aiterm
    /// cannot pre-mint one — its placeholder row is a tab handle for the life
    /// of the tab, and no panel is keyed to it.
    fn mints_session_id(&self) -> bool {
        false
    }

    /// Read from Codex's own cache rather than hardcoded.
    ///
    /// `~/.codex/models_cache.json` is written by the CLI and carries the slug,
    /// display name, the reasoning levels *each model* supports and its
    /// default — which is exactly this list, kept current by the tool itself.
    /// Absent (never run, or a future version that stops writing it) the list
    /// is empty and the UI offers only the agent default, which is correct
    /// rather than a guess.
    fn models(&self) -> Vec<ModelOption> {
        codex_models().unwrap_or_default()
    }

    fn permission_modes(&self) -> &'static [PermissionMode] {
        CODEX_PERMISSION_MODES
    }

    /// Effort is a config override, not a flag: `codex --help` has no effort
    /// option, and `model_reasoning_effort` is a real config key — verified
    /// 2026-07-27 in the native binary of codex-cli 0.145.0.
    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut cmd = String::from("codex");
        if let Some(m) = spec.model.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        if let Some(e) = spec.effort.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" -c model_reasoning_effort={}", q(e)));
        }
        // spec.session_id is deliberately dropped — see mints_session_id.
        if let Some(p) = prompt_of(spec) {
            cmd.push_str(&format!(" {}", q(p)));
        }
        cmd
    }
}

pub struct OpenCodeBackend;

impl AgentBackend for OpenCodeBackend {
    fn id(&self) -> &'static str {
        "opencode"
    }
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }
    fn detect(&self) -> Detection {
        Detection { caps: self.caps(), ..detect_cli(self.id(), self.display_name(), "opencode") }
    }
    fn sessions(&self) -> &dyn SessionProvider {
        &crate::opencode::OpencodeSessions
    }

    /// Resume and delete, and nothing else.
    ///
    /// There is a session reader now (`opencode.rs`), so a row exists to point
    /// ▶ at. The rest stay false for the same reasons as before: OpenCode has
    /// no fork, takes no id up front so there is nothing to re-key, renders its
    /// own screen in a way `term/screen.ts` was never written for, and keeps no
    /// transcript in the shape the panels parse.
    ///
    /// `delete` is claimed even though `find_session_file` answers with
    /// `opencode.db` — the one database holding every OpenCode conversation on
    /// the machine — because `session_delete` never renames an OpenCode
    /// session: it branches to `opencode::delete_to_trash`, which dumps the
    /// session's rows to the trash and deletes exactly those rows. The
    /// file-move path cannot reach this backend's path.
    fn caps(&self) -> Caps {
        Caps { resume: true, delete: true, ..Default::default() }
    }

    /// `opencode --help`: `-s, --session <id>` reopens a stored session. The id
    /// is quoted like every other value that reaches `$SHELL -ic`.
    ///
    /// The model rides along when the database knows it, because `--session`
    /// alone reopens the conversation on OpenCode's *default* model — measured
    /// 2026-08-10, when a GLM session resumed as Llama-4-Scout.
    fn resume(&self, session_id: &str) -> Option<String> {
        Some(opencode_resume_command(
            session_id,
            crate::opencode::session_model(session_id),
        ))
    }

    fn api_resume_context(&self, session_id: &str) -> Option<(String, String)> {
        crate::opencode::session_model(session_id)
    }

    /// `opencode --help` names sessions only to `--continue`/`--session` back
    /// into existing ones; nothing pre-mints an id.
    fn mints_session_id(&self) -> bool {
        false
    }

    /// The startup shortlist, spelled the way OpenCode wants it. This is the
    /// answer to OpenCode's own model picker being a chore: star models once
    /// in Model access and they appear here, prefixed for its catalog.
    fn models(&self) -> Vec<ModelOption> {
        opencode_models(&crate::providers::load_providers())
    }

    fn permission_modes(&self) -> &'static [PermissionMode] {
        OPENCODE_PERMISSION_MODES
    }

    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut cmd = String::from("opencode");
        if let Some(m) = spec.model.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        // No effort concept, and no pre-minted id — see mints_session_id.
        if let Some(p) = prompt_of(spec) {
            cmd.push_str(&format!(" --prompt {}", q(p)));
        }
        cmd
    }

    /// Not a tab of its own: picking a Model-access model is how an OpenCode
    /// session starts.
    fn offered(&self) -> bool {
        false
    }

    /// OpenRouter, plus any provider OpenCode's own config already declares.
    /// OpenCode keys its catalog by *its* provider ids, so an arbitrary
    /// OpenAI-compatible endpoint would launch a session that fails to
    /// resolve — but an endpoint that opencode.json itself lists (matched by
    /// base URL) resolves by that config's id. On this machine that is the
    /// local llama router: aiterm provider base http://localhost:8099/v1,
    /// opencode.json provider "local" at http://127.0.0.1:8099/v1.
    /// [observed: opencode.json, 2026-08-31]
    fn accepts_api(&self, provider: &crate::providers::Provider) -> bool {
        provider.is_openrouter() || opencode_provider_matching(provider).is_some()
    }

    /// Its catalog prefix, matching the shortlist `opencode_models` builds —
    /// or the opencode.json provider id for an endpoint that config declares.
    fn api_model_slug(&self, provider: &crate::providers::Provider, model_id: &str) -> String {
        if provider.is_openrouter() {
            return format!("openrouter/{model_id}");
        }
        match opencode_provider_matching(provider) {
            Some((ocid, _)) => format!("{ocid}/{model_id}"),
            None => model_id.to_string(),
        }
    }

    /// It authenticates from `OPENROUTER_API_KEY` in its environment; there is
    /// no other way to hand it a key that aiterm holds.
    fn needs_provider_key_env(&self) -> bool {
        true
    }
}

/// `aiterm chat` — this binary's own console harness, as a backend.
///
/// The id `"api"` is load-bearing twice over: it is stamped on every chat row
/// already in the index (changing it orphans them) and it is now this
/// backend's key in the registry. Neither can be renamed without the other.
///
/// It is always available — it is this executable — and never offered in the
/// ＋ menu: a chat starts by picking a model in Model access, and a second
/// door to the same room would be confusing rather than convenient.
pub struct ChatBackend;

/// The chats `aiterm chat` writes: one jsonl per conversation under
/// `~/.local/share/aiterm/chats`.
pub struct ChatSessions;

impl SessionProvider for ChatSessions {
    fn scan_with_paths(&self) -> Vec<(Session, std::path::PathBuf)> {
        crate::chat::scan_chats()
    }
    fn scan_with_paths_bounded(
        &self,
        budget: &mut crate::sessions::DiscoveryBudget,
    ) -> Vec<(Session, std::path::PathBuf)> {
        crate::chat::scan_chats_bounded(budget)
    }
    fn find_session_file(&self, session_id: &str) -> Option<std::path::PathBuf> {
        crate::chat::chat_file_if_exists(session_id)
    }
}

/// This binary's path, shell-quoted, for a command that re-enters it in `chat`
/// argv mode.
///
/// `current_exe` can fail — a deleted or replaced binary mid-run — and the
/// launch string has nowhere to report that, so fall back to the bare name and
/// let the shell's PATH lookup answer. An installed aiterm resolves; one run
/// from a build directory gets a shell error naming the command, which is a
/// better failure than a tab that opens on nothing.
fn chat_exe() -> String {
    q(&std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "aiterm".into()))
}

impl AgentBackend for ChatBackend {
    fn id(&self) -> &'static str {
        "api"
    }
    fn display_name(&self) -> &'static str {
        "aiterm chat"
    }

    /// No PATH probe: the harness is this process. Version and path are `None`
    /// because neither describes a separate program.
    fn detect(&self) -> Detection {
        Detection {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            available: true,
            version: None,
            path: None,
            caps: self.caps(),
        }
    }

    fn sessions(&self) -> &dyn SessionProvider {
        &ChatSessions
    }

    /// The id is aiterm's own: the transcript is named after it before the
    /// first token arrives, so the sidebar row is real from the first frame.
    fn mints_session_id(&self) -> bool {
        true
    }

    /// Reached through the Model-access dropdown, not the ＋ menu.
    fn offered(&self) -> bool {
        false
    }

    /// Resume and delete. There is no fork, no re-key, no TUI to drive, and the
    /// transcript panels read Claude Code's record types, not ours.
    ///
    /// `delete` because this is the one non-claude backend whose files are
    /// aiterm's own: one `~/.local/share/aiterm/chats/<id>.jsonl` per chat,
    /// written by this binary, one session per file. Trashing one takes exactly
    /// that conversation and nothing else.
    fn caps(&self) -> Caps {
        Caps { resume: true, delete: true, ..Default::default() }
    }

    /// The fallback, so it declines nothing. If it did, an API model on a
    /// provider no engine wanted would resolve to no plan at all.
    fn accepts_api(&self, _provider: &crate::providers::Provider) -> bool {
        true
    }

    /// The key is deliberately absent: this process reads the provider store
    /// itself, so nothing secret needs to cross the PTY environment.
    fn launch(&self, spec: &LaunchSpec) -> String {
        let mut cmd = format!("{} chat", chat_exe());
        if let Some(p) = spec.provider.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --provider {}", q(p)));
        }
        if let Some(m) = spec.model.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        if let Some(id) = spec.session_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.push_str(&format!(" --session-id {}", q(id)));
        }
        if let Some(p) = prompt_of(spec) {
            cmd.push_str(&format!(" --prompt {}", q(p)));
        }
        cmd
    }

    /// Provider and model come from the transcript itself, so a resume names
    /// only the chat.
    fn resume(&self, session_id: &str) -> Option<String> {
        Some(format!("{} chat --resume {}", chat_exe(), q(session_id)))
    }
}

/// The resume command, with the session's model spelled the way OpenCode
/// wants it — `providerID/modelID`, the same shape its `-m` takes everywhere.
/// Split out from [`AgentBackend::resume`] so the shape is testable without a
/// database on the machine.
fn opencode_resume_command(session_id: &str, model: Option<(String, String)>) -> String {
    match model {
        Some((provider, model)) => format!(
            "opencode --session {} --model {}",
            q(session_id),
            q(&format!("{provider}/{model}")),
        ),
        None => format!("opencode --session {}", q(session_id)),
    }
}

/// The startup shortlist as OpenCode model slugs (`openrouter/<vendor>/<id>`).
///
/// Only providers that actually are OpenRouter map — OpenCode's catalog keys
/// models by *its* provider ids, and pretending an arbitrary OpenAI-compatible
/// endpoint is one of them would launch sessions that fail to resolve. A
/// custom endpoint needs an entry in opencode.json, which is config aiterm
/// does not write behind anyone's back.
pub fn opencode_models(providers: &[crate::providers::Provider]) -> Vec<ModelOption> {
    providers
        .iter()
        .filter(|p| p.is_openrouter())
        .flat_map(|p| {
            p.startup_models.iter().map(|m| ModelOption {
                id: format!("openrouter/{m}"),
                display_name: m.clone(),
                efforts: vec![],
                default_effort: None,
            })
        })
        .collect()
}

/// Parse `~/.codex/models_cache.json`. Split out so the shape can be tested
/// against a captured copy without needing Codex installed.
fn codex_models() -> Option<Vec<ModelOption>> {
    let path = dirs::home_dir()?.join(".codex/models_cache.json");
    let text = std::fs::read_to_string(path).ok()?;
    parse_codex_models(&text)
}

pub fn parse_codex_models(text: &str) -> Option<Vec<ModelOption>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let models = v.get("models")?.as_array()?;
    let out: Vec<ModelOption> = models
        .iter()
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?.to_string();
            let display_name = m
                .get("display_name")
                .and_then(|d| d.as_str())
                .unwrap_or(&id)
                .to_string();
            let efforts = m
                .get("supported_reasoning_levels")
                .and_then(|l| l.as_array())
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|l| {
                            // Entries are objects with an `effort`, but accept a
                            // bare string too rather than dropping the list if a
                            // future version simplifies it.
                            l.get("effort")
                                .and_then(|e| e.as_str())
                                .or_else(|| l.as_str())
                                .map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(ModelOption {
                id,
                display_name,
                efforts,
                default_effort: m
                    .get("default_reasoning_level")
                    .and_then(|d| d.as_str())
                    .map(String::from),
            })
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Every backend aiterm knows about.
///
/// One registry, so a new agent is added in one place and cannot be half-added
/// — the previous arrangement built this list inline in `list_sessions` while
/// the indexer named `ClaudeProvider` directly, which is exactly the shape that
/// gets you rows you cannot search.
/// Order is behaviour, not taste: it is the tie-break for session lookup and
/// the priority for "who runs this API model". `ChatBackend` sits last because
/// it accepts everything — anything after it would never be asked.
pub fn backends() -> Vec<Box<dyn AgentBackend>> {
    vec![
        Box::new(ClaudeBackend),
        Box::new(CodexBackend),
        Box::new(crate::grok::GrokBackend),
        Box::new(OpenCodeBackend),
        Box::new(crate::antigravity::AntigravityBackend),
        Box::new(ChatBackend),
    ]
}

/// Every backend's sessions with their transcript paths, newest first.
///
/// The single entry point for "what sessions exist" — listing and indexing both
/// come through here, so a backend cannot be visible in one and absent from the
/// other.
///
/// Each row is stamped with the id of the backend that produced it, rather than
/// trusting the parser to label its own output. The parser is per-agent and its
/// label would be a second place for the name to live; this way `agent` cannot
/// disagree with the registry, which is what the UI switches on.
///
/// Every source comes through the registry, API chats included — they used to
/// be appended here by hand, which is one more list to remember and the reason
/// a chat could appear twice the moment it also became a backend.
pub fn scan_all_with_paths() -> Vec<(Session, std::path::PathBuf)> {
    scan_backends(&backends())
}

/// The body of [`scan_all_with_paths`], over an explicit list.
///
/// Split out so the composition rules — tagging, global ordering — can be
/// tested against fake backends. With one real backend in the registry there is
/// otherwise nothing to compose, and the interesting behaviour would go
/// unexercised until the day a second one is added.
fn scan_backends(list: &[Box<dyn AgentBackend>]) -> Vec<(Session, std::path::PathBuf)> {
    let mut budget = crate::sessions::DiscoveryBudget::new();
    let mut all: Vec<(Session, std::path::PathBuf)> = list
        .iter()
        .flat_map(|b| {
            let id = b.id();
            b.sessions()
                .scan_with_paths_bounded(&mut budget)
                .into_iter()
                .map(move |(mut s, path)| {
                    s.agent = id.to_string();
                    (s, path)
                })
        })
        .collect();
    // Sorted across all backends, not within each: the list is one timeline of
    // your work, and grouping it by which tool happened to produce a row would
    // be an odd thing to impose on it.
    all.sort_by(|a, b| b.0.last_active.cmp(&a.0.last_active));
    all
}

/// The transcript for `session_id`, from whichever backend owns it.
///
/// Ownership is decided by asking, not by inspecting the id: ids are opaque,
/// and a rule for telling one agent's from another's would be a guess that
/// breaks the first time a format changes. First backend to find the file wins,
/// which makes registry order the tie-break — see the id-collision test for why
/// that is stated rather than left to chance.
pub fn find_session_file_in(
    list: &[Box<dyn AgentBackend>],
    session_id: &str,
) -> Option<std::path::PathBuf> {
    owner_in(list, session_id).map(|(_, path)| path)
}

/// The backend that owns `session_id`, and where it says the session lives.
///
/// The one ownership lookup: [`find_session_file_in`], `launch.rs`'s resolver,
/// the preview panel and `session_delete` all come through here, so they cannot
/// disagree about who owns a row. Both halves of the answer are returned
/// because every caller wants one or the other and asking twice would run the
/// lookup twice — for OpenCode that is a `sqlite3` spawn.
///
/// A path is not always a transcript. OpenCode answers with its database, so a
/// caller that means to *read* the conversation must ask
/// [`crate::sessions::SessionProvider::messages`] first.
pub fn owner_in<'a>(
    list: &'a [Box<dyn AgentBackend>],
    session_id: &str,
) -> Option<(&'a dyn AgentBackend, std::path::PathBuf)> {
    list.iter()
        .find_map(|b| b.sessions().find_session_file(session_id).map(|p| (&**b, p)))
}

/// The conversation `agent_id` holds for `session_id`, when that backend keeps
/// its sessions somewhere other than a file. `None` everywhere else, meaning
/// "read the transcript".
///
/// Keyed by agent id rather than by [`owner_in`] because the caller — the
/// search indexer — is walking rows that already carry the id of the backend
/// that produced them. Asking the registry to rediscover that per row would
/// spawn a lookup per session for no new information.
pub fn backend_messages(agent_id: &str, session_id: &str) -> Option<Vec<(String, String)>> {
    let list = backends();
    let b = list.iter().find(|b| b.id() == agent_id)?;
    b.sessions().messages(session_id)
}

/// What aiterm can see on this machine, in registry order.
///
/// Reports every known backend, present or not: "Codex — not installed" is
/// more useful in a settings panel than an absence, and it is the difference
/// between a tool aiterm does not support and one you have not installed.
/// `async` because detection spawns processes. Each absent backend costs a
/// bounded login-shell probe and each present one a `--version` run, so this
/// can take seconds on a cold PATH; a plain `#[tauri::command]` runs on the
/// main thread and would freeze the window for every one of them. Same reason
/// `usage.rs` and `fonts.rs` are `async`.
#[tauri::command(async)]
pub fn detect_agents(
    services: tauri::State<'_, crate::services::ApplicationServices>,
) -> Vec<Detection> {
    detect_agents_from(services.inner())
}

#[doc(hidden)]
pub fn detect_agents_from(services: &crate::services::ApplicationServices) -> Vec<Detection> {
    services.agents.detect()
}

/// What every registered engine supports, keyed by id.
///
/// The same flags [`detect_agents`] already carries, without any of its cost.
/// That one probes PATH and runs `<bin> --version` through a login shell — the
/// reason it is `async` — and the UI gates a *render* on these: which buttons a
/// sidebar row offers, and whether the claude-shaped subsystems run against the
/// active tab. Waiting seconds on a cold PATH to find out whether to draw a ⑂
/// would be absurd, so this asks the registry and nothing else.
///
/// Deliberately not `async`, against the rule that commands go off the main
/// thread: `id()` and `caps()` are constant functions over a `vec!` of unit
/// structs, so the thread hop would cost more than the work. Every backend
/// appears, present or not — a row from an uninstalled engine still has to know
/// what it can offer.
#[tauri::command]
pub fn agent_caps(
    services: tauri::State<'_, crate::services::ApplicationServices>,
) -> std::collections::HashMap<String, Caps> {
    agent_caps_from(services.inner())
}

#[doc(hidden)]
pub fn agent_caps_from(
    services: &crate::services::ApplicationServices,
) -> std::collections::HashMap<String, Caps> {
    services.agents.caps()
}

/// What a new session can be started as: the agents that are actually here,
/// with their models. Absent agents are omitted — this feeds a picker, and
/// offering something that cannot start is worse than not offering it.
#[derive(Serialize, Clone, Debug)]
pub struct AgentChoice {
    pub id: String,
    pub display_name: String,
    pub models: Vec<ModelOption>,
    pub mints_session_id: bool,
}

/// `async` for the same reason as [`detect_agents`], plus `models()`, which for
/// Codex shells out again to read its model list.
#[tauri::command(async)]
pub fn agent_choices(
    services: tauri::State<'_, crate::services::ApplicationServices>,
) -> Vec<AgentChoice> {
    agent_choices_from(services.inner())
}

#[doc(hidden)]
pub fn agent_choices_from(
    services: &crate::services::ApplicationServices,
) -> Vec<AgentChoice> {
    services.agents.list()
}

/// The id of the session `agent_id` just started in `cwd`, once it exists.
///
/// An agent that has no `--session-id` cannot be told what to call itself, so
/// aiterm cannot key a tab to it at launch the way it does for claude. What it
/// can do is watch: Codex writes its transcript — session id and cwd included —
/// the moment it starts, so the session that appears in the directory we
/// launched in is ours. Adopting it turns the placeholder row into the real
/// one, which is what stops a Codex conversation owning two sidebar rows: an
/// unretirable placeholder and the scanned row for the same transcript.
///
/// `known` is the set of ids that already existed when the terminal opened.
/// Identity by absence rather than by timestamp: mtime moves when an old
/// session is merely appended to, and adopting a conversation the user is
/// still using would be far worse than adopting nothing.
#[tauri::command(async)]
pub fn adopt_agent_session(
    agent_id: String,
    cwd: String,
    since_ms: u64,
    known: Vec<String>,
) -> Option<String> {
    let list = backends();
    let backend = list.iter().find(|b| b.id() == agent_id)?;
    pick_adopted(&backend.sessions().scan(), &cwd, since_ms, &known)
}

/// The body of [`adopt_agent_session`], over an explicit session list.
///
/// Split out because the interesting part is which row gets chosen, and that
/// is untestable through the command: it would need a real agent to have
/// really started. Adopting the wrong session would silently repoint a tab at
/// somebody else's conversation, so this is worth pinning down.
fn pick_adopted(
    sessions: &[Session],
    cwd: &str,
    since_ms: u64,
    known: &[String],
) -> Option<String> {
    let known: std::collections::HashSet<&str> = known.iter().map(String::as_str).collect();
    let fresh = |s: &&Session| s.last_active >= since_ms && !known.contains(s.id.as_str());
    sessions
        .iter()
        .filter(|s| s.project_path == cwd && fresh(s))
        // Newest, for the case where a directory gains two sessions inside one
        // poll interval. Ours is the one that started later.
        .max_by_key(|s| s.last_active)
        // An engine that does not record where it ran (antigravity before a
        // command runs; whatever comes next) yields a row with no folder at
        // all. Such a row cannot belong to a directory we did not launch in,
        // so a fresh one is ours by the same absence-not-timestamp argument:
        // it did not exist when the tab opened. A row with SOME OTHER folder
        // is still never taken.
        .or_else(|| {
            sessions
                .iter()
                .filter(|s| s.project_path.is_empty() && fresh(s))
                .max_by_key(|s| s.last_active)
        })
        .map(|s| s.id.clone())
}

fn pick_clear_successor(
    sessions: &[Session],
    previous_id: &str,
    cwd: &str,
    since_ms: u64,
    known: &[String],
) -> Option<String> {
    let known: std::collections::HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut candidates = sessions.iter().filter(|s| {
        s.id != previous_id
            && s.project_path == cwd
            && s.last_active >= since_ms
            && !known.contains(s.id.as_str())
    });
    let candidate = candidates.next()?;
    candidates.next().is_none().then(|| candidate.id.clone())
}

/// The session Codex created after an explicit `/clear` in an existing tab.
///
/// Codex has no SessionStart hook for an in-place clear: it changes its own
/// session id while keeping the same PTY alive. The terminal supplies the
/// exact clear submission and the session ids visible at that instant; this
/// command accepts only one newly-written row, never a "newest wins" guess.
#[tauri::command(async)]
pub fn clear_successor_session(
    agent_id: String,
    previous_id: String,
    cwd: String,
    since_ms: u64,
    known: Vec<String>,
) -> Option<String> {
    let list = backends();
    let backend = list.iter().find(|b| b.id() == agent_id)?;
    pick_clear_successor(
        &backend.sessions().scan(),
        &previous_id,
        &cwd,
        since_ms,
        &known,
    )
}

/// Resolve `bin` against PATH, the way a shell would.
///
/// Deliberately not `which`/`command -v`: spawning a shell to ask whether a
/// program exists costs more than the answer, and would make "is Codex
/// installed?" a process spawn per backend per call.
pub(crate) fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|candidate| is_executable_file(candidate))
}

/// Ask the user's login shell where `bin` is.
///
/// Necessary because aiterm's own PATH is not the one you get at a prompt. A
/// desktop launcher starts the app from the session manager with a minimal
/// environment, and Node version managers make it worse: fnm, nvm and asdf put
/// their shims on PATH from a shell rc file, and fnm's live in a per-shell
/// directory (`/run/user/…/fnm_multishells/<pid>_<ts>/bin`) that does not exist
/// outside one.
///
/// That is not a corner case for this feature — Codex is a Node CLI, so an fnm
/// user has it installed and aiterm would flatly report "not installed".
/// *Observed 2026-07-27: `codex-cli 0.145.0` resolving only inside a shell.*
///
/// Interactive as well as login (`-lic`), because rc files that set these shims
/// up are usually the interactive ones. Bounded, because an interactive shell
/// can block on anything a user has put in their profile, and a settings panel
/// that never finishes loading is no better than one that hangs.
pub(crate) fn which_via_login_shell(bin: &str) -> Option<std::path::PathBuf> {
    let shell = std::env::var("SHELL").ok()?;
    // `bin` is a literal from our own backend list, never user input, but keep
    // the quoting correct anyway rather than relying on that staying true.
    let script = format!("command -v '{}' 2>/dev/null", bin.replace('\'', "'\\''"));
    let out = run_bounded(&shell, &["-l", "-i", "-c", &script], Duration::from_secs(4))?;
    // An interactive shell may print a banner or an rc-file warning first, so
    // take the last line that is actually a path to an executable rather than
    // assuming the output is clean. It must also be named `bin`: a banner line
    // that happens to name some other executable is not an answer to the
    // question, and accepting it would mean launching the wrong program.
    String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .map(|l| std::path::PathBuf::from(l.trim()))
        .find(|p| p.file_name().is_some_and(|n| n == bin) && is_executable_file(p))
}

/// Run a command, giving up after `limit`. Returns stdout on success.
///
/// `std::process::Command` has no timeout, and a shell that hangs on someone's
/// profile would hang the settings panel with it. On timeout the worker thread
/// is left to finish on its own — it holds nothing but its own stdout buffer,
/// and abandoning it is better than blocking the UI on it.
pub(crate) fn run_bounded(program: &str, args: &[&str], limit: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    std::thread::spawn(move || {
        let result = std::process::Command::new(&program).args(&args).output();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(out)) if out.status.success() => Some(out.stdout),
        _ => None,
    }
}

#[cfg(unix)]
fn is_executable_file(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Detection for a backend that is a command-line program.
///
/// A missing binary is the ordinary case, not an error: most machines will have
/// one of these agents and not the others, and that is worth showing plainly
/// rather than treating as a failure.
pub(crate) fn detect_cli(id: &str, display_name: &str, bin: &str) -> Detection {
    // PATH first because it is free; the shell only when that fails, so the
    // common case never spawns anything to answer "is it installed".
    let found = which(bin).or_else(|| which_via_login_shell(bin));
    let version = found.as_ref().and_then(|p| read_version(p));
    Detection {
        id: id.to_string(),
        display_name: display_name.to_string(),
        available: found.is_some(),
        version,
        path: found.map(|p| p.to_string_lossy().into_owned()),
        // Nothing to declare from a PATH probe: what an engine supports is the
        // backend's own answer, filled in by whoever called this.
        caps: Caps::default(),
    }
}

/// First line of `<bin> --version`, or `None` if it failed or said nothing.
///
/// Some tools print their version to stderr, so both streams are considered.
/// A tool that is installed but will not report a version is still usable, so
/// this never affects `available`.
fn read_version(bin: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(bin).arg("--version").output().ok()?;
    let text = if out.stdout.is_empty() { &out.stderr } else { &out.stdout };
    String::from_utf8_lossy(text)
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The permission flags are no longer baked into `launch()` — they are the
    /// setting the resolver appends — so `launch()` is bare, and the flags the
    /// panel shows are the first mode's until a choice is stored. This pins the
    /// default so nobody's sessions change: claude's first mode is exactly the
    /// invocation aiterm always ran.
    #[test]
    fn claudes_default_mode_is_the_invocation_aiterm_always_ran() {
        let first = CLAUDE_PERMISSION_MODES.first().expect("claude has modes");
        assert_eq!(first.id, "auto");
        assert_eq!(
            first.flags,
            &["--permission-mode auto", "--allow-dangerously-skip-permissions"],
        );
    }

    #[test]
    fn only_an_engine_with_a_config_surface_offers_one() {
        assert!(ClaudeBackend.caps().config, "claude has settings.json to show");
        assert!(!CodexBackend.caps().config, "nothing is read for codex yet");
    }

    #[test]
    fn which_finds_a_program_that_exists_and_misses_one_that_does_not() {
        assert!(which("sh").is_some(), "sh should be on PATH");
        assert!(
            which("definitely-not-a-real-binary-aiterm").is_none(),
            "invented a program that is not installed",
        );
    }

    #[test]
    fn which_ignores_directories_and_unexecutable_files() {
        // A directory named like the binary must not count as finding it.
        let dir = std::env::temp_dir().join("aiterm-which-test");
        let _ = std::fs::create_dir_all(dir.join("notabin"));
        assert!(!is_executable_file(&dir.join("notabin")), "a directory passed as executable");
    }

    /// The case that actually happens: an agent the user has not installed.
    /// It must report cleanly rather than erroring, because a settings panel
    /// showing "Codex — not installed" is the whole point.
    #[test]
    fn a_missing_cli_detects_as_unavailable_without_failing() {
        let d = detect_cli("ghost", "Ghost Agent", "definitely-not-a-real-binary-aiterm");
        assert!(!d.available);
        assert_eq!(d.version, None);
        assert_eq!(d.path, None);
        assert_eq!(d.display_name, "Ghost Agent");
    }

    /// `sh` stands in for an installed agent: present on PATH, with a resolved
    /// path. Whether it reports a `--version` is not asserted — some tools do
    /// not, which is exactly why `available` does not depend on it.
    #[test]
    fn an_installed_cli_detects_as_available_with_a_path() {
        let d = detect_cli("sh", "Bourne Shell", "sh");
        assert!(d.available, "sh was not detected");
        assert!(d.path.is_some_and(|p| p.ends_with("sh")));
    }

    #[test]
    fn every_backend_reports_its_own_identity() {
        for b in backends() {
            let d = b.detect();
            assert_eq!(d.id, b.id(), "detection reported a different id");
            assert_eq!(d.display_name, b.display_name());
            assert!(!d.id.is_empty() && !d.display_name.is_empty());
        }
    }

    /* ---- composition, against fake backends ----------------------------- */

    struct FakeProvider {
        /// (session id, last_active) — enough to test tagging and ordering.
        rows: Vec<(&'static str, u64)>,
    }

    impl SessionProvider for FakeProvider {
        fn scan_with_paths(&self) -> Vec<(Session, std::path::PathBuf)> {
            self.rows
                .iter()
                .map(|(id, at)| {
                    (
                        Session {
                            id: (*id).to_string(),
                            // Deliberately wrong: the registry must stamp this,
                            // not trust what the provider labelled it.
                            agent: "WRONG".into(),
                            title: (*id).to_string(),
                            project_path: "/p".into(),
                            group_path: "/p".into(),
                            branch: None,
                            forked: false,
                            background: false,
                            fork_parent: None,
                            last_active: *at,
                        },
                        std::path::PathBuf::from(format!("/fake/{id}")),
                    )
                })
                .collect()
        }
        fn find_session_file(&self, session_id: &str) -> Option<std::path::PathBuf> {
            self.rows
                .iter()
                .any(|(id, _)| *id == session_id)
                .then(|| std::path::PathBuf::from(format!("/fake/{session_id}")))
        }

        fn scan_with_paths_bounded(
            &self,
            budget: &mut crate::sessions::DiscoveryBudget,
        ) -> Vec<(Session, std::path::PathBuf)> {
            let take = budget.remaining().min(self.rows.len());
            let rows = self.rows[..take]
                .iter()
                .map(|(id, at)| {
                    (
                        Session {
                            id: (*id).to_string(),
                            agent: "WRONG".into(),
                            title: (*id).to_string(),
                            project_path: "/p".into(),
                            group_path: "/p".into(),
                            branch: None,
                            forked: false,
                            background: false,
                            fork_parent: None,
                            last_active: *at,
                        },
                        std::path::PathBuf::from(format!("/fake/{id}")),
                    )
                })
                .collect();
            for _ in 0..take {
                let _ = budget.claim_file();
            }
            rows
        }
    }

    struct FakeBackend {
        id: &'static str,
        provider: FakeProvider,
    }

    impl AgentBackend for FakeBackend {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.id
        }
        fn detect(&self) -> Detection {
            Detection {
                id: self.id.to_string(),
                display_name: self.id.to_string(),
                available: true,
                version: None,
                path: None,
                caps: Caps::default(),
            }
        }
        fn sessions(&self) -> &dyn SessionProvider {
            &self.provider
        }
        fn launch(&self, _spec: &LaunchSpec) -> String {
            format!("fake-{}", self.id)
        }
    }

    fn fake(id: &'static str, rows: Vec<(&'static str, u64)>) -> Box<dyn AgentBackend> {
        Box::new(FakeBackend { id, provider: FakeProvider { rows } })
    }

    #[test]
    fn production_composition_applies_one_global_discovery_budget_across_providers() {
        let list = vec![
            fake("first", vec![("a", 1); 3_000]),
            fake("second", vec![("b", 2); 3_000]),
        ];
        let rows = scan_backends(&list);
        assert_eq!(rows.len(), crate::sessions::MAX_DISCOVERED_SESSION_FILES);
        assert_eq!(rows.iter().filter(|(session, _)| session.agent == "first").count(), 3_000);
        assert_eq!(rows.iter().filter(|(session, _)| session.agent == "second").count(), 1_096);
    }

    /// The whole point of the registry: two agents, one list.
    #[test]
    fn a_codex_rollout_header_yields_a_listable_session() {
        // Shape taken from a real rollout written by codex-cli 0.145.0 on
        // 2026-07-28, trimmed to the fields the sidebar reads.
        let line = r#"{"timestamp":"2026-07-29T00:43:28.854Z","type":"session_meta",
            "payload":{"session_id":"019fab53-92d3-7c10-91a5-e3bc82210418",
            "cwd":"/home/m/Projects/opcode","originator":"codex_cli_rs",
            "git":{"commit_hash":"abc123","branch":"main"}}}"#;
        assert_eq!(
            parse_codex_header(line),
            Some(CodexHeader {
                id: "019fab53-92d3-7c10-91a5-e3bc82210418".into(),
                cwd: "/home/m/Projects/opcode".into(),
                branch: Some("main".into()),
            })
        );
    }

    #[test]
    fn a_rollout_with_no_git_still_lists() {
        // Codex outside a repository omits the git block entirely; a session
        // in a plain directory is still a session.
        let line = r#"{"payload":{"session_id":"abc","cwd":"/tmp/scratch"}}"#;
        assert_eq!(
            parse_codex_header(line),
            Some(CodexHeader {
                id: "abc".into(),
                cwd: "/tmp/scratch".into(),
                branch: None,
            })
        );
    }

    #[test]
    fn anything_that_is_not_a_rollout_header_is_skipped() {
        // A half-written first line, and a well-formed line of some other
        // shape. Both must be skipped rather than panicking a scan that runs
        // every time the sidebar refreshes.
        assert_eq!(parse_codex_header(""), None);
        assert_eq!(parse_codex_header("{\"payload\":{\"cwd\":\"/x\"}}"), None);
        assert_eq!(parse_codex_header("{\"timestamp\":\"2026"), None);
    }

    /// A delete has to take the whole conversation, so this must find every
    /// rollout sharing the session id, including child rollouts which do not
    /// appear as session rows.
    #[test]
    fn a_conversations_rollouts_are_all_found_for_the_trash() {
        use std::io::Write;
        use std::time::{Duration, UNIX_EPOCH};
        let dir = std::env::temp_dir().join("aiterm-codex-files-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, sid: &str, secs: u64| {
            let p = dir.join(name);
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "{{\"payload\":{{\"session_id\":\"{sid}\",\"cwd\":\"/home/m/p\"}}}}").unwrap();
            f.set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
            p
        };
        let first = write("rollout-a-1.jsonl", "sess-1", 100);
        let third = write("rollout-c-3.jsonl", "sess-1", 300);
        let second = write("rollout-b-2.jsonl", "sess-1", 200);
        write("rollout-d-4.jsonl", "sess-2", 400);

        assert_eq!(
            codex_session_files_in(&dir, "sess-1"),
            vec![first, second, third],
            "every rollout of the conversation, oldest first, and nobody else's",
        );
        assert!(codex_session_files_in(&dir, "sess-nothing").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Legacy rollouts with only session_id remain deduplicated by that id.
    /// A second, separate session stays its own row.
    #[test]
    fn many_rollouts_of_one_session_collapse_to_the_newest() {
        use std::io::Write;
        use std::time::{Duration, UNIX_EPOCH};
        let dir = std::env::temp_dir().join("aiterm-codex-dedup-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, sid: &str, secs: u64| -> std::path::PathBuf {
            let p = dir.join(name);
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "{{\"payload\":{{\"session_id\":\"{sid}\",\"cwd\":\"/home/m/proj\"}}}}").unwrap();
            f.set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
            p
        };
        // Three files of one conversation with ascending mtimes, plus a
        // separate session. The middle-written file is the newest by mtime.
        write("rollout-a-111.jsonl", "sess-1", 100);
        let newest = write("rollout-b-222.jsonl", "sess-1", 300);
        write("rollout-c-333.jsonl", "sess-1", 200);
        write("rollout-d-444.jsonl", "sess-2", 50);

        let mut rows = scan_codex_dir(&dir);
        rows.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        assert_eq!(rows.len(), 2, "two sessions, not four files");
        assert_eq!(rows[0].0.id, "sess-1");
        assert_eq!(rows[0].1, newest, "the surviving file is the newest of the session");
        assert_eq!(rows[1].0.id, "sess-2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_roots_keep_their_own_identity_transcript_and_timestamp() {
        use std::io::Write;
        use std::time::{Duration, UNIX_EPOCH};
        let dir = std::env::temp_dir().join(format!("aiterm-codex-roots-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |id: &str, family: &str, source: serde_json::Value, thread: &str, secs: u64| {
            let path = dir.join(format!("rollout-{id}.jsonl"));
            let mut file = std::fs::File::create(&path).unwrap();
            let header = serde_json::json!({"type":"session_meta","payload":{
                "id":id,"session_id":family,"cwd":"/home/m/proj",
                "source":source,"thread_source":thread
            }});
            writeln!(file, "{header}").unwrap();
            file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
            path
        };
        let old = write("old", "old", "cli".into(), "user", 100);
        // A retained family id must not merge a new user root into the old row.
        let cleared = write("cleared", "old", "cli".into(), "user", 200);
        let child = write(
            "child",
            "old",
            serde_json::json!({"subagent":{}}),
            "subagent",
            400,
        );
        let child_cli = write("child-cli", "old", "cli".into(), "subagent", 500);
        let mut rows = scan_codex_dir(&dir);
        rows.sort_by(|a, b| b.0.last_active.cmp(&a.0.last_active));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (&rows[0].0.id, &rows[0].1, rows[0].0.last_active),
            (&"cleared".to_owned(), &cleared, 200_000)
        );
        assert_eq!(
            (&rows[1].0.id, &rows[1].1, rows[1].0.last_active),
            (&"old".to_owned(), &old, 100_000)
        );
        assert_eq!(
            codex_session_files_in(&dir, "old"),
            vec![old, child, child_cli]
        );
        assert_eq!(codex_session_files_in(&dir, "cleared"), vec![cleared]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_header_uses_id_without_requiring_a_family_id() {
        let header = r#"{"type":"session_meta","payload":{"id":"root","cwd":"/tmp","source":"cli","thread_source":"user"}}"#;
        assert_eq!(parse_codex_header(header).unwrap().id, "root");
        let child = r#"{"payload":{"id":"child","session_id":"root","cwd":"/tmp","source":{"subagent":{}}}}"#;
        assert_eq!(parse_codex_header(child), None);
    }

    /// A session row with only the fields adoption looks at.
    fn row(id: &str, cwd: &str, at: u64) -> Session {
        Session {
            id: id.into(),
            agent: "codex".into(),
            title: id.into(),
            project_path: cwd.into(),
            group_path: cwd.into(),
            branch: None,
            forked: false,
            background: false,
            fork_parent: None,
            last_active: at,
        }
    }

    #[test]
    fn adoption_takes_the_session_that_appeared_where_we_launched() {
        let rows = vec![
            row("old", "/workspace/project", 100),
            row("new", "/workspace/project", 500),
        ];
        assert_eq!(
            pick_adopted(&rows, "/workspace/project", 400, &["old".into()]),
            Some("new".into())
        );
    }

    #[test]
    fn adoption_never_takes_a_session_that_was_already_there() {
        // The whole hazard: an old conversation in the same directory that the
        // user is still typing into has a fresh mtime, and stealing it would
        // repoint the tab at their work.
        let rows = vec![row("busy", "/workspace/project", 9_999)];
        assert_eq!(
            pick_adopted(&rows, "/workspace/project", 400, &["busy".into()]),
            None
        );
    }

    #[test]
    fn adoption_ignores_other_directories_and_stale_rows() {
        let rows = vec![
            row("elsewhere", "/home/m/aiterm", 500),
            row("stale", "/workspace/project", 100),
        ];
        assert_eq!(pick_adopted(&rows, "/workspace/project", 400, &[]), None);
    }

    #[test]
    fn adoption_takes_a_fresh_row_that_names_no_folder_at_all() {
        // agy writes no workspace for a plain chat, so the row has no folder.
        // It appeared after we launched and was not there before: ours.
        let rows = vec![
            row("old-nowhere", "", 100),
            row("new-nowhere", "", 500),
            row("elsewhere", "/home/m/aiterm", 600),
        ];
        assert_eq!(
            pick_adopted(&rows, "/workspace/project", 400, &["old-nowhere".into()]),
            Some("new-nowhere".into())
        );
        // A row that names our folder still wins over one that names none.
        let rows = vec![row("here", "/workspace/project", 450), row("nowhere", "", 500)];
        assert_eq!(pick_adopted(&rows, "/workspace/project", 400, &[]), Some("here".into()));
        // Known or stale folder-less rows are left alone, like any other.
        let rows = vec![row("busy", "", 9_999), row("stale", "", 100)];
        assert_eq!(pick_adopted(&rows, "/workspace/project", 400, &["busy".into()]), None);
    }

    #[test]
    fn adoption_finds_nothing_before_the_agent_has_written_anything() {
        assert_eq!(pick_adopted(&[], "/workspace/project", 400, &[]), None);
    }

    #[test]
    fn clear_successor_adopts_the_only_new_session_in_its_terminal_directory() {
        // If `/clear` changes a Codex TUI from `before` to `after`, the tab
        // must follow `after`; otherwise the new row looks unowned and a click
        // tries to resume a conversation that the existing terminal still has
        // open.
        let rows = vec![
            row("before", "/workspace/project", 100),
            row("after", "/workspace/project", 500),
            row("elsewhere", "/home/m/aiterm", 600),
        ];
        assert_eq!(
            pick_clear_successor(&rows, "before", "/workspace/project", 400, &["before".into()]),
            Some("after".into()),
        );
    }

    #[test]
    fn clear_successor_refuses_an_ambiguous_new_session() {
        // A second terminal can start Codex in the same directory at the same
        // time. Re-keying either tab to a guess would swap their conversations.
        let rows = vec![
            row("before", "/workspace/project", 100),
            row("first", "/workspace/project", 500),
            row("second", "/workspace/project", 600),
        ];
        assert_eq!(
            pick_clear_successor(&rows, "before", "/workspace/project", 400, &["before".into()]),
            None,
        );
    }

    #[test]
    fn sessions_from_every_backend_appear_in_one_list() {
        let list = vec![fake("claude", vec![("c1", 10)]), fake("codex", vec![("x1", 20)])];
        let ids: Vec<String> = scan_backends(&list).into_iter().map(|(s, _)| s.id).collect();
        assert_eq!(ids.len(), 2, "a backend's sessions went missing");
        assert!(ids.contains(&"c1".to_string()) && ids.contains(&"x1".to_string()));
    }

    /// The registry is the source of truth for `agent`, not the parser. The UI
    /// switches its icon on this field, and a provider mislabelling its own
    /// output would be a second place for the name to live.
    #[test]
    fn every_row_is_tagged_with_the_backend_that_produced_it() {
        let list = vec![fake("claude", vec![("c1", 10)]), fake("codex", vec![("x1", 20)])];
        for (s, _) in scan_backends(&list) {
            let expected = if s.id == "c1" { "claude" } else { "codex" };
            assert_eq!(s.agent, expected, "row {} carried the wrong agent", s.id);
        }
    }

    /// One timeline of your work, not blocks grouped by tool. If ordering were
    /// per-backend, the newest session could sit below a week-old one purely
    /// because of which agent produced it.
    #[test]
    fn ordering_is_global_and_interleaves_backends() {
        let list = vec![
            fake("claude", vec![("old", 10), ("newest", 40)]),
            fake("codex", vec![("newer", 30), ("oldest", 5)]),
        ];
        let ids: Vec<String> = scan_backends(&list).into_iter().map(|(s, _)| s.id).collect();
        assert_eq!(ids, vec!["newest", "newer", "old", "oldest"]);
    }

    #[test]
    fn a_transcript_is_found_through_the_backend_that_owns_it() {
        let list = vec![fake("claude", vec![("c1", 10)]), fake("codex", vec![("x1", 20)])];
        assert_eq!(
            find_session_file_in(&list, "x1"),
            Some(std::path::PathBuf::from("/fake/x1")),
            "did not route to the owning backend",
        );
        assert_eq!(find_session_file_in(&list, "nobody"), None);
    }

    /// Ids are opaque and separately generated, so two agents *could* mint the
    /// same one. Nothing merges or dedupes them — both rows stay, each tagged
    /// with its own agent — and lookup resolves in registry order. Pinned here
    /// so the behaviour is a decision rather than an accident.
    #[test]
    fn colliding_ids_across_backends_stay_separate_rows() {
        let list = vec![fake("claude", vec![("same", 10)]), fake("codex", vec![("same", 20)])];
        let rows = scan_backends(&list);
        assert_eq!(rows.len(), 2, "rows from different agents were merged");
        let agents: Vec<String> = rows.into_iter().map(|(s, _)| s.agent).collect();
        assert!(agents.contains(&"claude".to_string()) && agents.contains(&"codex".to_string()));
        assert_eq!(
            find_session_file_in(&list, "same"),
            Some(std::path::PathBuf::from("/fake/same")),
            "lookup should resolve, first backend in registry order winning",
        );
    }

    #[test]
    fn codex_rollouts_become_sessions_and_junk_is_skipped() {
        // Codex files rollouts by date, so the scan has to walk rather than
        // list; the nesting here is the real layout.
        let root = std::env::temp_dir().join(format!("aiterm-codex-{}", std::process::id()));
        let day = root.join("2026/07/28");
        std::fs::create_dir_all(&day).expect("temp dir");
        std::fs::write(
            day.join("rollout-2026-07-28T20-43-28-aaa.jsonl"),
            "{\"payload\":{\"session_id\":\"aaa\",\"cwd\":\"/workspace/project\",\
             \"git\":{\"branch\":\"main\"}}}\n{\"more\":\"lines\"}\n",
        )
        .unwrap();
        // Not a rollout: must be skipped, not panicked on.
        std::fs::write(day.join("notes.jsonl"), "half a line, tru").unwrap();
        // Not even a .jsonl.
        std::fs::write(day.join("README"), "hello").unwrap();

        let found = scan_codex_dir(&root);
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(found.len(), 1, "one real rollout, two decoys");
        let (s, path) = &found[0];
        assert_eq!(s.id, "aaa");
        assert_eq!(s.project_path, "/workspace/project");
        // Titled by the working-directory basename.
        assert_eq!(s.title, "project");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(path.ends_with("rollout-2026-07-28T20-43-28-aaa.jsonl"));
    }

    #[test]
    fn a_missing_codex_directory_is_not_an_error() {
        // Most machines have never run Codex. That is an empty list, not a
        // failure that takes the whole sidebar down with it.
        assert!(scan_codex_dir(std::path::Path::new("/nonexistent/codex")).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn production_codex_discovery_rejects_a_symlink_provider_root() {
        use std::os::unix::fs::symlink;

        let fixture = std::env::temp_dir().join(format!(
            "aiterm-codex-discovery-root-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let outside = fixture.join("outside");
        std::fs::create_dir_all(outside.join("2026/08/29")).unwrap();
        let sentinel = outside.join("2026/08/29/rollout-sentinel.jsonl");
        std::fs::write(
            &sentinel,
            "{\"payload\":{\"session_id\":\"outside\",\"cwd\":\"/outside\"}}\n",
        )
        .unwrap();
        let linked_root = fixture.join("sessions");
        symlink(&outside, &linked_root).unwrap();

        assert!(scan_codex_dir(&linked_root).is_empty());
        assert!(sentinel.exists());
        std::fs::remove_dir_all(fixture).unwrap();
    }

    /// The registry must report agents that are absent, not omit them — the
    /// settings panel exists to say "not installed".
    #[test]
    fn detection_covers_every_registered_backend() {
        let found = detect_agents_from(&crate::services::ApplicationServices::desktop());
        assert_eq!(found.len(), backends().len(), "a backend went unreported");
        assert!(found.iter().any(|d| d.id == "claude"));
        assert!(found.iter().any(|d| d.id == "codex"));
    }

    /* ---- launch commands ------------------------------------------------ */

    /// `launch()` no longer carries permission flags — the resolver appends the
    /// chosen mode's — so with nothing picked the command is just the program
    /// plus, once the app has written its settings file, the hook `--settings`
    /// flag (environmental, hence the strip rather than an exact match).
    #[test]
    fn claude_launch_is_bare_of_permission_flags() {
        let cmd = ClaudeBackend.launch(&LaunchSpec::default());
        assert!(!cmd.contains("--permission-mode"), "{cmd}");
        assert!(!cmd.contains("skip-permissions"), "{cmd}");
        let stripped = match crate::hooklink::settings_flag() {
            Some(flag) => cmd.replace(&flag, ""),
            None => cmd,
        };
        assert_eq!(stripped, "claude");
    }

    /// OpenCode is an engine, not a menu entry: the Model-access dropdown is
    /// its door, so agent_choices must never offer it as a source.
    #[test]
    fn opencode_is_never_offered_as_a_source() {
        assert!(agent_choices_from(&crate::services::ApplicationServices::desktop())
            .iter()
            .all(|c| c.id != "opencode"));
    }

    /// OpenCode launches bare or with a model slug; a minted session id must
    /// be dropped (nothing pre-mints one there), never passed through.
    #[test]
    fn opencode_takes_a_model_and_drops_a_minted_id() {
        assert_eq!(OpenCodeBackend.launch(&LaunchSpec::default()), "opencode");
        let cmd = OpenCodeBackend.launch(&LaunchSpec {
            model: Some("openrouter/anthropic/claude-sonnet-5".into()),
            session_id: Some("abc-123".into()),
            ..Default::default()
        });
        assert_eq!(cmd, "opencode --model 'openrouter/anthropic/claude-sonnet-5'");
    }

    /// The startup shortlist becomes OpenCode's model list — OpenRouter
    /// providers only, prefixed for its catalog; a custom endpoint's models
    /// must not be dressed up as slugs OpenCode cannot resolve.
    #[test]
    fn opencode_models_come_from_openrouter_startup_lists_only() {
        let providers = vec![
            crate::providers::Provider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: "k".into(),
                management_key: String::new(),
                startup_models: vec!["anthropic/claude-sonnet-5".into(), "qwen/q3:free".into()],
                policy: Default::default(),
                routes: Default::default(),
            },
            crate::providers::Provider {
                id: "local".into(),
                name: "Local".into(),
                base_url: "http://localhost:8080/v1".into(),
                api_key: "k".into(),
                management_key: String::new(),
                startup_models: vec!["some-local-model".into()],
                policy: Default::default(),
                routes: Default::default(),
            },
        ];
        let models = opencode_models(&providers);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "openrouter/anthropic/claude-sonnet-5");
        assert_eq!(models[1].id, "openrouter/qwen/q3:free");
    }

    #[test]
    fn claude_takes_model_effort_and_a_minted_session_id() {
        let cmd = ClaudeBackend.launch(&LaunchSpec {
            model: Some("opus".into()),
            effort: Some("high".into()),
            session_id: Some("abc-123".into()),
            ..Default::default()
        });
        assert!(cmd.contains("--model 'opus'"), "{cmd}");
        assert!(cmd.contains("--effort 'high'"), "{cmd}");
        assert!(cmd.contains("--session-id 'abc-123'"), "{cmd}");
    }

    /// Codex has no --session-id, so a minted one must be dropped rather than
    /// passed as some other flag or silently appended as a prompt.
    #[test]
    fn codex_uses_a_config_override_for_effort_and_ignores_session_id() {
        let cmd = CodexBackend.launch(&LaunchSpec {
            model: Some("gpt-5.6-sol".into()),
            effort: Some("high".into()),
            session_id: Some("abc-123".into()),
            ..Default::default()
        });
        assert!(cmd.starts_with("codex "), "{cmd}");
        assert!(cmd.contains("--model 'gpt-5.6-sol'"), "{cmd}");
        assert!(cmd.contains("-c model_reasoning_effort='high'"), "{cmd}");
        assert!(!cmd.contains("abc-123"), "session id leaked into a codex launch: {cmd}");
    }

    /// The home screen's first message rides on the command, in each engine's
    /// spelling, shell-quoted, and after everything else — a blank one adds
    /// nothing at all.
    #[test]
    fn a_first_prompt_is_the_last_thing_on_the_line() {
        let spec = LaunchSpec {
            model: Some("opus".into()),
            prompt: Some("fix the thing, it's broken".into()),
            ..Default::default()
        };
        let claude = ClaudeBackend.launch(&spec);
        assert!(claude.ends_with(" 'fix the thing, it'\\''s broken'"), "{claude}");
        assert_eq!(CodexBackend.launch(&spec), "codex --model 'opus' 'fix the thing, it'\\''s broken'");
        assert_eq!(
            OpenCodeBackend.launch(&spec),
            "opencode --model 'opus' --prompt 'fix the thing, it'\\''s broken'"
        );
        assert!(ChatBackend.launch(&spec).ends_with("--prompt 'fix the thing, it'\\''s broken'"));
        let blank = LaunchSpec { prompt: Some("   ".into()), ..Default::default() };
        assert_eq!(CodexBackend.launch(&blank), "codex");
    }

    #[test]
    fn empty_choices_add_no_flags() {
        let spec = LaunchSpec {
            model: Some(String::new()),
            effort: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(CodexBackend.launch(&spec), "codex");
        assert!(!ClaudeBackend.launch(&spec).contains("--model"));
    }

    /// Resume must carry the same permission mode and hook settings as a start
    /// — it is the same session continuing, and a resume that ran under
    /// different flags would behave differently for no reason a user could see.
    #[test]
    fn claude_resumes_with_the_flags_it_starts_with() {
        let start = ClaudeBackend.launch(&LaunchSpec::default());
        let resumed = ClaudeBackend.resume("abc-123").expect("claude declined to resume");
        assert_eq!(resumed, format!("{start} --resume 'abc-123'"));
    }

    /// A `/clear` re-key is a fresh start under an id aiterm already knows, so
    /// it is exactly the launch command with that id in it.
    #[test]
    fn claude_clears_by_starting_fresh_under_the_given_id() {
        let cleared = ClaudeBackend.clear("abc-123").expect("claude declined to clear");
        assert_eq!(
            cleared,
            ClaudeBackend.launch(&LaunchSpec {
                session_id: Some("abc-123".into()),
                ..Default::default()
            })
        );
        assert!(cleared.contains("--session-id 'abc-123'"), "{cleared}");
    }

    /// Nobody but claude re-keys, because nobody else takes an id up front.
    #[test]
    fn engines_that_cannot_rekey_a_session_say_so() {
        assert_eq!(CodexBackend.clear("abc-123"), None);
        assert_eq!(OpenCodeBackend.clear("abc-123"), None);
        assert_eq!(ChatBackend.clear("abc-123"), None);
    }

    /// Codex reopens by UUID through its own `resume` subcommand — the whole of
    /// the fix, now that a Codex row's id is the conversation's session_id and
    /// the non-`tui_drive` branch carries that id straight through to the tab.
    #[test]
    fn codex_resumes_by_session_uuid() {
        assert_eq!(
            CodexBackend.resume("019ffbf3-142f-7750").as_deref(),
            Some("codex resume '019ffbf3-142f-7750'"),
        );
    }

    /// `opencode --help`: `-s, --session <id>`, `-m <provider/model>`. Quoted,
    /// like everything else that reaches `$SHELL -ic`. The pure helper is what
    /// gets asserted — `resume()` itself consults the live database, so its
    /// answer for a real id depends on the machine.
    #[test]
    fn opencode_reopens_a_session_by_id_and_names_its_model() {
        assert_eq!(
            opencode_resume_command(
                "ses_03ee94418ffeDX6l2Xs5hpVzcN",
                Some(("openrouter".into(), "z-ai/glm-5.2".into())),
            ),
            "opencode --session 'ses_03ee94418ffeDX6l2Xs5hpVzcN' --model 'openrouter/z-ai/glm-5.2'",
        );
        // No recorded model — a session that never got a reply — resumes bare
        // rather than inventing one.
        assert_eq!(
            opencode_resume_command("ses_03ee94418ffeDX6l2Xs5hpVzcN", None),
            "opencode --session 'ses_03ee94418ffeDX6l2Xs5hpVzcN'",
        );
        // An id that is not OpenCode's shape never reaches the database, so
        // this stays deterministic through the real `resume()`.
        assert_eq!(
            OpenCodeBackend.resume("a'b"),
            Some(r"opencode --session 'a'\''b'".into()),
        );
    }

    /// The renderer sends only what it picked. A spec that names a model and
    /// nothing else has to keep parsing — every field it leaves out is one the
    /// agent's own default answers.
    #[test]
    fn a_spec_naming_only_a_model_still_parses() {
        let spec: LaunchSpec = serde_json::from_str(r#"{"model":"opus"}"#).expect("spec");
        assert_eq!(spec.model.as_deref(), Some("opus"));
        assert_eq!(spec.provider, None);
        assert_eq!(spec.session_id, None);
    }

    /* ---- aiterm chat ---------------------------------------------------- */

    /// The harness runs this binary in its `chat` argv mode. The provider id
    /// is on the line; the key never is — `aiterm chat` reads the provider
    /// store itself, and `/proc` publishes argv to every process on the box.
    #[test]
    fn a_chat_launch_names_the_provider_and_never_the_key() {
        let cmd = ChatBackend.launch(&LaunchSpec {
            model: Some("anthropic/claude-sonnet-5".into()),
            session_id: Some("abc-123".into()),
            provider: Some("openrouter".into()),
            ..Default::default()
        });
        assert!(cmd.contains(" chat --provider 'openrouter'"), "{cmd}");
        assert!(cmd.contains("--model 'anthropic/claude-sonnet-5'"), "{cmd}");
        assert!(cmd.contains("--session-id 'abc-123'"), "{cmd}");
    }

    #[test]
    fn a_chat_resume_names_only_the_session() {
        let cmd = ChatBackend.resume("abc-123").expect("chat declined to resume");
        assert!(cmd.contains(" chat --resume 'abc-123'"), "{cmd}");
        assert!(!cmd.contains("--provider"), "provider guessed on a resume: {cmd}");
    }

    /// It is this executable, so it is always here — and it is the fallback
    /// for API models, which fails if it can ever be unavailable.
    #[test]
    fn the_chat_harness_is_always_available_and_never_offered() {
        let d = ChatBackend.detect();
        assert!(d.available);
        assert_eq!(d.id, "api");
        assert!(!ChatBackend.offered());
        assert!(agent_choices_from(&crate::services::ApplicationServices::desktop())
            .iter()
            .all(|c| c.id != "api"));
    }

    /// Chats reach the sidebar through the registry now. They used to be
    /// appended to the scan by hand as well, and both routes at once lists
    /// every chat twice.
    #[test]
    fn chats_come_from_exactly_one_place() {
        assert_eq!(
            backends().iter().filter(|b| b.id() == "api").count(),
            1,
            "more than one backend claims the chat store",
        );
        // Whatever is on this machine, no transcript may be listed twice: a
        // duplicate (id, path) pair can only come from two sources yielding the
        // same file. Vacuous on a machine with no sessions, and that is fine —
        // the assertion is about composition, not about content.
        let mut seen = std::collections::HashSet::new();
        for (s, path) in scan_all_with_paths() {
            assert!(
                seen.insert((s.id.clone(), path.clone())),
                "{} was listed twice from {}",
                s.id,
                path.display(),
            );
        }
    }

    /* ---- capabilities --------------------------------------------------- */

    /// The flags exist so the UI can stop comparing ids. Claude is the engine
    /// every gated subsystem was written against; everything else declares
    /// less rather than inheriting its buttons.
    #[test]
    fn capabilities_ride_on_detection_and_match_the_backend() {
        for b in backends() {
            assert_eq!(b.detect().caps, b.caps(), "{} detected different caps", b.id());
        }
        assert_eq!(
            ClaudeBackend.caps(),
            Caps {
                fork: true,
                clear: true,
                resume: true,
                tui_drive: true,
                panels: true,
                tasks: true,
                delete: true,
                config: true,
                roster_liveness: true,
            },
        );
        assert_eq!(
            CodexBackend.caps(),
            Caps { resume: true, delete: true, tasks: true, ..Default::default() }
        );
        assert_eq!(
            OpenCodeBackend.caps(),
            Caps { resume: true, delete: true, ..Default::default() },
        );
        assert_eq!(
            ChatBackend.caps(),
            Caps { resume: true, delete: true, ..Default::default() },
        );
    }

    /// The 🗑 is only offered where the engine has a delete that takes exactly
    /// one session.
    ///
    /// Claude's transcripts, aiterm's own chat jsonls and Codex's rollouts all
    /// have one file per conversation, which trash and restore move as a unit
    /// — a rollout goes back to Codex's own store, not claude's. OpenCode's
    /// "file" is `opencode.db`, which is *every* OpenCode conversation at once
    /// — its claim rides on `session_delete` branching to the row-level
    /// `opencode::delete_to_trash` instead of the rename, and this test is the
    /// reminder that removing that branch means removing the claim.
    /// Only claude has a roster to be absent from. The green dot asks this
    /// before deciding whether an open tab counts as proof a session is alive:
    /// for claude it must not (a tab can hold a session the daemon moved on
    /// from), and for everyone else it is the only proof available — without
    /// which a running Codex session showed no dot at all.
    #[test]
    fn only_claude_has_an_external_liveness_roster() {
        assert!(ClaudeBackend.caps().roster_liveness);
        assert!(!CodexBackend.caps().roster_liveness);
        assert!(!OpenCodeBackend.caps().roster_liveness);
        assert!(!ChatBackend.caps().roster_liveness);
    }

    #[test]
    fn only_an_engine_with_a_single_session_delete_offers_one() {
        assert!(ClaudeBackend.caps().delete);
        assert!(ChatBackend.caps().delete);
        assert!(CodexBackend.caps().delete, "a rollout is one session's file");
        assert!(OpenCodeBackend.caps().delete, "row-level delete via opencode::delete_to_trash");
    }

    /// The UI gates on this map, so a backend missing from it is an engine
    /// whose tabs silently get no buttons and none of its subsystems. Adding
    /// one to `backends()` without its capabilities reaching the renderer is
    /// exactly the half-added state the registry exists to prevent.
    #[test]
    fn every_backend_publishes_its_capabilities_to_the_ui() {
        let map = agent_caps_from(&crate::services::ApplicationServices::desktop());
        for b in backends() {
            assert_eq!(
                map.get(b.id()),
                Some(&b.caps()),
                "{} is invisible to the UI's capability lookup",
                b.id(),
            );
        }
        assert_eq!(map.len(), backends().len(), "two backends share an id");
    }

    /// A backend that offers ▶ must have a command behind it, and one that
    /// does not must not — a button with no command is a black pane.
    #[test]
    fn every_backend_that_claims_a_lifecycle_button_can_produce_one() {
        for b in backends() {
            assert_eq!(
                b.caps().resume,
                b.resume("abc-123").is_some(),
                "{} disagrees with itself about resume",
                b.id(),
            );
            assert_eq!(
                b.caps().clear,
                b.clear("abc-123").is_some(),
                "{} disagrees with itself about clear",
                b.id(),
            );
        }
    }

    /// OpenCode always takes OpenRouter and may take endpoints declared in the
    /// user's OpenCode config. An unrelated endpoint still reaches the chat
    /// harness, which is the all-accepting last resort.
    #[test]
    fn an_undeclared_provider_is_left_for_the_chat_harness() {
        let openrouter = crate::providers::Provider {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "k".into(),
            management_key: String::new(),
            startup_models: vec![],
            policy: Default::default(),
            routes: Default::default(),
        };
        let undeclared = crate::providers::Provider {
            id: "unit-test-undeclared".into(),
            name: "Unit test undeclared provider".into(),
            base_url: "https://unit-test-undeclared.invalid/v1".into(),
            ..openrouter.clone()
        };
        assert!(OpenCodeBackend.accepts_api(&openrouter));
        assert!(!OpenCodeBackend.accepts_api(&undeclared));
        assert!(ChatBackend.accepts_api(&openrouter) && ChatBackend.accepts_api(&undeclared));
        assert!(!ClaudeBackend.accepts_api(&openrouter));
        assert!(!CodexBackend.accepts_api(&openrouter));
    }

    /// The catalog prefix is OpenCode's spelling and nobody else's — the
    /// harness passes the provider's own model id straight through.
    #[test]
    fn only_opencode_prefixes_an_api_model_id() {
        let or = crate::providers::Provider {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "k".into(),
            management_key: String::new(),
            startup_models: vec![],
            policy: Default::default(),
            routes: Default::default(),
        };
        assert_eq!(OpenCodeBackend.api_model_slug(&or, "a/b"), "openrouter/a/b");
        assert_eq!(ChatBackend.api_model_slug(&or, "a/b"), "a/b");
        assert_eq!(ClaudeBackend.api_model_slug(&or, "a/b"), "a/b");
    }

    /// The opencode.json bridge: a provider whose endpoint OpenCode's own
    /// config declares is accepted and spelled by that config's id — with
    /// localhost and 127.0.0.1 folded together, the exact mismatch between
    /// this machine's provider store and its opencode.json.
    /// [observed: opencode.json, 2026-08-31]
    #[test]
    fn opencode_json_providers_parse_and_match() {
        let cfg = r#"{
            "provider": {
                "local": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "baseURL": "http://127.0.0.1:8099/v1",
                        "apiKey": "{env:LOCAL_LLM_API_KEY}"
                    },
                    "models": {"qwen3.8-27b": {}}
                }
            }
        }"#;
        let parsed = parse_opencode_providers(cfg);
        assert_eq!(parsed.len(), 1);
        let (id, base, env) = &parsed[0];
        assert_eq!(id, "local");
        assert!(same_base_url(base, "http://localhost:8099/v1"));
        assert!(same_base_url(base, "http://127.0.0.1:8099/v1/"));
        assert!(!same_base_url(base, "http://localhost:8098/v1"));
        assert_eq!(env.as_deref(), Some("LOCAL_LLM_API_KEY"));
        // A literal key is not an env spelling and names no variable.
        let literal = r#"{"provider":{"x":{"options":{"baseURL":"http://h/v1","apiKey":"sk-abc"}}}}"#;
        assert_eq!(parse_opencode_providers(literal)[0].2, None);
    }

    /// The key goes only where it is the only way in. The harness reads the
    /// provider store itself, so handing it one would widen exposure for
    /// nothing.
    #[test]
    fn only_an_engine_with_no_other_way_in_asks_for_the_key() {
        assert!(OpenCodeBackend.needs_provider_key_env());
        assert!(!ChatBackend.needs_provider_key_env());
        assert!(!ClaudeBackend.needs_provider_key_env());
    }

    /// These strings are handed to `$SHELL -ic`. The UI only sends values from
    /// lists we produced, but that is not a boundary to rely on.
    ///
    /// Asserted by asking a real shell rather than by pattern-matching the
    /// command text: what matters is that the shell sees one literal word, and
    /// only the shell can settle that. (An earlier version of this test looked
    /// for the payload as a substring and failed on correctly-quoted output —
    /// the payload is *supposed* to be in there, inside the quotes.)
    #[test]
    fn values_survive_the_shell_as_a_single_literal_word() {
        for nasty in [
            "x'; touch /tmp/aiterm-pwned; #",
            "$(touch /tmp/aiterm-pwned)",
            "a b\tc",
            "back\\slash",
        ] {
            let out = std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {}", q(nasty))])
                .output()
                .expect("run sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                nasty,
                "the shell did not see {nasty:?} as one literal word",
            );
        }
        assert!(
            !std::path::Path::new("/tmp/aiterm-pwned").exists(),
            "quoting failed: the payload executed",
        );
    }

    /* ---- codex model cache ---------------------------------------------- */

    /// Captured from a real `~/.codex/models_cache.json` (codex-cli 0.145.0).
    const CODEX_CACHE: &str = r#"{
      "fetched_at": "2026-07-28T00:22:46Z",
      "models": [
        {"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol",
         "default_reasoning_level":"low",
         "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"max"}]},
        {"slug":"gpt-5.6-codex","display_name":"GPT-5.6-Codex",
         "supported_reasoning_levels":["medium"]}
      ]}"#;

    #[test]
    fn codex_models_come_from_its_own_cache() {
        let models = parse_codex_models(CODEX_CACHE).expect("parsed nothing");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert_eq!(models[0].display_name, "GPT-5.6-Sol");
        assert_eq!(models[0].efforts, vec!["low", "high", "max"]);
        assert_eq!(models[0].default_effort.as_deref(), Some("low"));
        // Bare strings accepted too, so a simplified future format still parses.
        assert_eq!(models[1].efforts, vec!["medium"]);
        assert_eq!(models[1].default_effort, None);
    }

    /// A missing or unreadable cache must yield "we do not know" rather than an
    /// invented list — the UI then offers only the agent's own default.
    #[test]
    fn an_unusable_codex_cache_yields_no_models() {
        assert_eq!(parse_codex_models("not json"), None);
        assert_eq!(parse_codex_models(r#"{"models":[]}"#), None);
        assert_eq!(parse_codex_models(r#"{"other":1}"#), None);
    }

    #[test]
    fn agent_choices_only_offers_agents_that_are_here() {
        for c in agent_choices_from(&crate::services::ApplicationServices::desktop()) {
            let backend = backends().into_iter().find(|b| b.id() == c.id).unwrap();
            assert!(backend.detect().available, "{} offered but not installed", c.id);
        }
    }

    #[test]
    fn an_empty_registry_is_not_an_error() {
        assert!(scan_backends(&[]).is_empty());
        assert_eq!(find_session_file_in(&[], "anything"), None);
    }

    #[test]
    fn backend_ids_are_unique() {
        let ids: Vec<&str> = backends().iter().map(|b| b.id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "two backends share an id: {ids:?}");
    }
}






#[cfg(test)]
mod codex_panel_tests {
    use super::*;

    #[test]
    fn a_js_plan_with_bare_keys_is_read() {
        let input = "const p = await tools.update_plan({plan:[\n  {step:\"Snapshot state\",status:\"in_progress\"},\n  {step:\"Implement [phase 2]\",status:\"pending\"}\n]}); text(p);\n";
        let plan = extract_js_plan(input).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], ("Snapshot state".to_string(), "in_progress".to_string()));
        assert_eq!(plan[1].0, "Implement [phase 2]", "brackets inside a step are not structure");
    }

    #[test]
    fn a_js_plan_with_quoted_keys_and_an_explanation_is_read() {
        let input = concat!(
            "const p = await tools.update_plan({explanation:\"baseline done\",",
            "\"plan\":[{\"step\":\"Copy new2 to new3\",\"status\":\"completed\"},",
            "{\"step\":\"Wire cancellation\",\"status\":\"in_progress\"}]}); text(p);"
        );
        let plan = extract_js_plan(input).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], ("Copy new2 to new3".to_string(), "completed".to_string()));
        assert_eq!(plan[1].1, "in_progress");
    }

    #[test]
    fn patch_paths_stop_at_the_escaped_newline() {
        // Inside an exec input the patch is a JS string literal, so its
        // newlines are the two characters backslash-n.
        let input = "const patch = \"*** Begin Patch\\n*** Add File: /tmp/a.md\\n+hello\\n*** Update File: /tmp/b.rs\\n+world\\n*** End Patch\";";
        let paths = patch_paths(input);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], ("/tmp/a.md".to_string(), "Write"));
        assert_eq!(paths[1], ("/tmp/b.rs".to_string(), "Edit"));
    }
}
