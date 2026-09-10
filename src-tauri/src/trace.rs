//! Verbose tracing — Express-middleware-style visibility into everything
//! aiterm does, switchable at runtime in every build.
//!
//! Why this exists alongside [`crate::diag`]: they answer different questions.
//! `diag!` is a small, always-on journal of *what aiterm did* — a handful of
//! lines a session, safe to ship, and the thing that has actually diagnosed
//! real faults. This is the firehose: every IPC call with its arguments, every
//! instrumented function with its timing. Useful while chasing something; far
//! too much to leave running, which is why it is a switch and not a default.
//!
//! **The switch works in release builds.** Matt tests from release RPMs, so a
//! debug-only firehose was a firehose he could never open (the first version
//! of this file compiled it all away with `release_max_level_off`). Now the
//! macros stay in every binary and a `reload`-able filter decides at runtime;
//! while the toggle is off each event site is one cached filter check, which
//! is as close to free as a shipped diagnostic gets.
//!
//! Where output goes: `~/.local/share/aiterm/trace.log`, truncated each time
//! the toggle turns on — stderr is a black hole under a desktop launcher, the
//! very lesson `diag.rs` was built on. Debug builds *also* mirror to stderr,
//! where `npm run tauri dev` can see it.
//!
//! Three ways in:
//! - the Diagnostics panel's "Verbose trace" toggle ([`trace_set`]),
//! - `AITERM_TRACE=<filter>` in the environment at launch (also chooses the
//!   filter, e.g. `aiterm::sessions=trace`),
//! - debug builds start with stderr tracing on (`aiterm=debug`) regardless.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter, Registry};

/// Whether the file capture is on — what the Diagnostics toggle shows.
static CAPTURING: AtomicBool = AtomicBool::new(false);

/// The trace.log sink, present while capture is on.
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Handle for swapping the filter at runtime.
static RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

fn trace_path() -> Option<std::path::PathBuf> {
    Some(dirs::data_dir()?.join("aiterm/trace.log"))
}

/// The filter to run when tracing is on: the environment's word, or a default
/// chatty on our crate and quiet on tauri/wry, whose debug output buries ours.
fn active_filter() -> EnvFilter {
    EnvFilter::try_from_env("AITERM_TRACE")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("aiterm=debug,warn"))
}

/// Everything off — the release-build resting state.
fn off_filter() -> EnvFilter {
    EnvFilter::new("off")
}

/// Where events go. One writer serving both destinations so a single fmt
/// layer covers every mode: trace.log while capturing, stderr in debug
/// builds always.
struct Sink;
struct SinkWriter;

impl std::io::Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if cfg!(debug_assertions) {
            let _ = std::io::stderr().write_all(buf);
        }
        if let Ok(mut guard) = FILE.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = FILE.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = SinkWriter;
    fn make_writer(&'a self) -> SinkWriter {
        SinkWriter
    }
}

/// Install the subscriber. Called once from `run()`.
///
/// The subscriber always exists; the filter decides whether it hears
/// anything. Debug builds start audible on stderr; release builds start
/// silent unless `AITERM_TRACE` is set, in which case capture begins at
/// launch — the only way to trace startup itself from an RPM install.
pub fn init() {
    use tracing_subscriber::fmt::format::FmtSpan;

    let env_asked = std::env::var_os("AITERM_TRACE").is_some();
    let start_on = cfg!(debug_assertions) || env_asked;

    let (filter_layer, handle) = reload::Layer::new(if start_on {
        active_filter()
    } else {
        off_filter()
    });

    // `FmtSpan::CLOSE` is what turns instrumented functions into timings: each
    // span reports its own elapsed time when it ends, which is the difference
    // between "these things happened" and "this is where the time went".
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::CLOSE)
        .with_target(true)
        .with_ansi(false)
        .with_writer(Sink);

    if Registry::default()
        .with(filter_layer)
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        return; // someone beat us to it (tests); leave theirs alone
    }
    let _ = RELOAD.set(handle);

    if env_asked {
        // Environment launch: open the file too, so a desktop-file user who
        // edits Exec= to add the variable still gets something readable.
        if open_capture_file() {
            CAPTURING.store(true, Ordering::Relaxed);
        }
    }
    tracing::debug!("tracing installed — toggle it in Settings → Diagnostics");
}

/// Truncate-and-open trace.log as the capture sink.
fn open_capture_file() -> bool {
    let Some(path) = trace_path() else {
        return false;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::File::create(&path) {
        Ok(f) => {
            if let Ok(mut guard) = FILE.lock() {
                *guard = Some(f);
            }
            true
        }
        Err(e) => {
            crate::diag!("trace", "couldn't open {}: {e}", path.display());
            false
        }
    }
}

/// Whether verbose capture is on, for the Diagnostics toggle to render.
#[tauri::command]
pub fn trace_status() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Turn verbose capture on or off. Returns the trace.log path when turning
/// on, so the panel can say where the output is going.
///
/// Truncates on every enable: a trace is for the thing being chased *now*,
/// and an unbounded append-forever firehose is a disk full of nothing.
#[tauri::command]
pub fn trace_set(on: bool) -> Result<Option<String>, String> {
    let handle = RELOAD.get().ok_or("tracing was never installed")?;
    if on {
        if !open_capture_file() {
            return Err("couldn't open trace.log".into());
        }
        handle.reload(active_filter()).map_err(|e| e.to_string())?;
        CAPTURING.store(true, Ordering::Relaxed);
        crate::diag!("trace", "verbose capture ON");
        tracing::info!("trace capture started ({})", env!("CARGO_PKG_VERSION"));
        Ok(trace_path().map(|p| p.to_string_lossy().into_owned()))
    } else {
        // Debug builds fall back to stderr chatter rather than to silence —
        // the toggle governs the file, not the dev console.
        let resting = if cfg!(debug_assertions) {
            active_filter()
        } else {
            off_filter()
        };
        handle.reload(resting).map_err(|e| e.to_string())?;
        CAPTURING.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = FILE.lock() {
            if let Some(mut f) = guard.take() {
                let _ = f.flush();
            }
        }
        crate::diag!("trace", "verbose capture off");
        Ok(None)
    }
}

/// Wrap the generated IPC handler so every call from the webview is logged
/// before it dispatches — the `morgan`/Express-middleware equivalent.
///
/// `tauri::ipc::Invoke` is documented as "used internally by macros and is
/// explicitly **NOT** stable", so this is the one place in aiterm that leans
/// on an unstable Tauri API. The blast radius is a compile error on a Tauri
/// bump, not a runtime fault. While the filter is off, the cost per call is
/// the `enabled!` check and nothing else — the argument formatting is behind
/// it.
///
/// Ordinary arguments are logged as bounded JSON. Commands that carry input,
/// terminal-derived output, credentials, or configuration contents are
/// summarized here instead. This boundary is fail-closed: adding fields to a
/// sensitive command cannot make their values appear in the trace.
pub fn log_invokes<R, F>(
    handler: F,
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
where
    R: tauri::Runtime,
    F: Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke| {
        if tracing::enabled!(target: "aiterm::ipc", tracing::Level::DEBUG) {
            let cmd = invoke.message.command().to_string();
            let args = match invoke.message.payload() {
                tauri::ipc::InvokeBody::Json(v) => format_json_invoke(&cmd, v),
                tauri::ipc::InvokeBody::Raw(bytes) => format!("<{} raw bytes>", bytes.len()),
            };
            tracing::debug!(target: "aiterm::ipc", "→ {cmd} {args}");
        }
        handler(invoke)
    }
}

fn format_json_invoke(command: &str, value: &serde_json::Value) -> String {
    match command {
        "tab_write" => {
            let tab_id = opaque_id(value, "tabId");
            let attachment_id = opaque_id(value, "attachmentId");
            let data = value
                .get("data")
                .and_then(serde_json::Value::as_str)
                .map(|data| format!("{} utf-8 bytes", data.len()))
                .unwrap_or_else(|| "redacted terminal input".to_string());
            format!("{{tabId:{tab_id}, attachmentId:{attachment_id}, data:<{data}>}}")
        }
        "provider_save" | "provider_management_key_set" => {
            "<provider credentials redacted>".to_string()
        }
        "claude_save_layer" | "claude_set_key" => "<configuration contents redacted>".to_string(),
        "write_text_file" | "render_markdown" => "<file contents redacted>".to_string(),
        "opencode_dispatch" => "<prompt contents redacted>".to_string(),
        "desktop_notify" | "tray_alerts" => {
            "<terminal-derived notification contents redacted>".to_string()
        }
        "ui_log" => "<renderer diagnostic message redacted>".to_string(),
        _ => bounded_json(value),
    }
}

fn opaque_id(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .unwrap_or("<invalid opaque id>")
        .to_string()
}

fn bounded_json(value: &serde_json::Value) -> String {
    let json = value.to_string();
    if json.len() <= 300 {
        return json;
    }
    let mut end = 300.min(json.len());
    while !json.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes)", &json[..end], json.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_input_is_never_formatted_into_the_invoke_trace() {
        let canary = "TERMINAL-SECRET-MUST-NEVER-REACH-TRACE";
        let rendered = format_json_invoke(
            "tab_write",
            &json!({
                "tabId": "550e8400-e29b-41d4-a716-446655440000",
                "attachmentId": "d9428888-122b-11e1-b85c-61cd3cbb3210",
                "data": canary,
            }),
        );

        assert!(
            !rendered.contains(canary),
            "terminal input leaked: {rendered}"
        );
        assert!(rendered.contains("550e8400-e29b-41d4-a716-446655440000"));
        assert!(rendered.contains("d9428888-122b-11e1-b85c-61cd3cbb3210"));
        assert!(rendered.contains(&canary.len().to_string()));

        let unicode = format_json_invoke(
            "tab_write",
            &json!({
                "tabId": "550e8400-e29b-41d4-a716-446655440000",
                "attachmentId": "d9428888-122b-11e1-b85c-61cd3cbb3210",
                "data": "🔐",
            }),
        );
        assert!(
            unicode.contains("4 utf-8 bytes"),
            "wrong byte length: {unicode}"
        );

        let invalid_ids = format_json_invoke(
            "tab_write",
            &json!({ "tabId": canary, "attachmentId": canary, "data": canary }),
        );
        assert!(
            !invalid_ids.contains(canary),
            "unvalidated identifiers bypassed fail-closed formatting: {invalid_ids}"
        );
    }

    #[test]
    fn sensitive_registered_commands_fail_closed_before_formatting() {
        let canary = "CREDENTIAL-OR-CONFIG-CANARY-MUST-NOT-APPEAR";
        for (command, payload) in [
            ("provider_save", json!({ "id": "host", "apiKey": canary })),
            (
                "provider_management_key_set",
                json!({ "id": "host", "key": canary }),
            ),
            (
                "claude_save_layer",
                json!({ "path": "/tmp/settings", "newText": canary, "loadedText": canary }),
            ),
            (
                "claude_set_key",
                json!({ "path": "/tmp/settings", "value": canary, "loadedText": canary }),
            ),
            (
                "write_text_file",
                json!({ "path": "/tmp/file", "content": canary }),
            ),
            (
                "desktop_notify",
                json!({ "summary": "terminal", "body": canary }),
            ),
            (
                "tray_alerts",
                json!({ "alerts": [{ "key": "tab", "message": canary }] }),
            ),
            ("opencode_dispatch", json!({ "prompt": canary })),
            ("ui_log", json!({ "msg": canary })),
        ] {
            let rendered = format_json_invoke(command, &payload);
            assert!(
                !rendered.contains(canary),
                "{command} leaked sensitive content: {rendered}"
            );
        }
    }
}
