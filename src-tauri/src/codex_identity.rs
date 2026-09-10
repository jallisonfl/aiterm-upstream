//! Resolve a running Codex terminal from the rollout its process actually owns.
//! Command-line resume ids become stale after `/clear`. Only open descriptors
//! establish ownership; root creation metadata orders retained cleared roots.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// Return the current user conversation held open by this PTY process tree.
/// Multiple owning processes or ambiguous root metadata yield no id.
pub(crate) fn resolve(root_pid: u32) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let sessions = dirs::home_dir()?.join(".codex/sessions");
    resolve_at(root_pid, Path::new("/proc"), &sessions)
}

fn resolve_at(root_pid: u32, proc_root: &Path, sessions: &Path) -> Option<String> {
    let sessions = fs::canonicalize(sessions).ok()?;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for entry in fs::read_dir(proc_root).ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        if let Some(parent) = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
        {
            children.entry(parent).or_default().push(pid);
        }
    }

    let mut pending = vec![root_pid];
    let mut visited = HashSet::new();
    let mut paths = HashSet::new();
    let mut owners = HashMap::<u32, Vec<RootSession>>::new();
    while let Some(pid) = pending.pop() {
        if !visited.insert(pid) {
            continue;
        }
        pending.extend(children.get(&pid).into_iter().flatten().copied());
        let process = proc_root.join(pid.to_string());
        let is_codex = fs::read_to_string(process.join("comm"))
            .is_ok_and(|comm| comm.trim() == "codex")
            || fs::read_link(process.join("exe"))
                .is_ok_and(|exe| exe.file_name().is_some_and(|name| name == "codex"));
        if !is_codex {
            continue;
        }
        // A live Codex process with inaccessible descriptors could own another
        // root conversation. Do not silently discard it and choose a sibling.
        for fd in fs::read_dir(process.join("fd")).ok()? {
            let fd = fd.ok()?;
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            if !target.starts_with(&sessions)
                || target.extension().is_none_or(|ext| ext != "jsonl")
                || !target
                    .file_name()?
                    .to_string_lossy()
                    .starts_with("rollout-")
            {
                continue;
            }
            let canonical = fs::canonicalize(&target).ok()?;
            if !canonical.starts_with(&sessions) || !paths.insert(canonical.clone()) {
                continue;
            }
            if let Some(id) = root_session(&canonical)? {
                owners.entry(pid).or_default().push(id);
            }
        }
    }
    // A /clear retains the previous root's writer in the same Codex process.
    // Do not confuse that with multiple independently running Codex instances.
    if owners.len() != 1 {
        return None;
    }
    let mut roots = owners.into_values().next()?;
    if roots.len() == 1 {
        return Some(roots.pop()?.id);
    }
    // Never rank by mtime: background completions can touch an old rollout.
    // Require creation metadata on every candidate and a unique newest root.
    if roots.iter().any(|root| root.created.is_none()) {
        return None;
    }
    roots.sort_by(|left, right| left.created.cmp(&right.created));
    let newest = roots.pop()?;
    if roots.last()?.created == newest.created {
        return None;
    }
    Some(newest.id)
}

// Outer None means unreadable/incomplete metadata, so ownership is unknown;
// Some(None) is a known non-user rollout that is safe to ignore.
struct RootSession {
    id: String,
    created: Option<String>,
}

fn root_session(path: &Path) -> Option<Option<RootSession>> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(MAX_HEADER_BYTES + 1));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.len() as u64 > MAX_HEADER_BYTES || !line.ends_with('\n') {
        return None;
    }
    let header: serde_json::Value = serde_json::from_str(&line).ok()?;
    if header.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = header.get("payload")?;
    if payload.get("source")?.as_str() != Some("cli") {
        return Some(None);
    }
    // Older CLI rollouts omit thread_source; newer user roots say "user".
    if payload
        .get("thread_source")
        .is_some_and(|source| source.as_str() != Some("user"))
    {
        return Some(None);
    }
    let id = payload.get("id")?.as_str()?;
    if id.is_empty() {
        return None;
    }
    let created = payload
        .get("timestamp")
        .and_then(|value| value.as_str())
        .filter(|value| {
            // Codex writes UTC with millisecond precision. Require that exact
            // form so lexical ordering cannot misorder offsets or precision.
            value.len() == 24
                && value.bytes().enumerate().all(|(i, b)| match i {
                    4 | 7 => b == b'-',
                    10 => b == b'T',
                    13 | 16 => b == b':',
                    19 => b == b'.',
                    23 => b == b'Z',
                    _ => b.is_ascii_digit(),
                })
        })
        .map(str::to_owned);
    Some(Some(RootSession {
        id: id.to_owned(),
        created,
    }))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aiterm-codex-identity-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(path.join("proc")).unwrap();
            fs::create_dir_all(path.join("sessions")).unwrap();
            Self(path)
        }
        fn process(&self, pid: u32, parent: u32, name: &str) {
            let path = self.0.join(format!("proc/{pid}"));
            fs::create_dir_all(path.join("fd")).unwrap();
            fs::write(path.join("status"), format!("PPid:\t{parent}\n")).unwrap();
            fs::write(path.join("comm"), name).unwrap();
        }
        fn rollout(&self, pid: u32, fd: u32, id: &str, source: serde_json::Value, thread: &str) {
            let path = self.0.join(format!("sessions/rollout-{id}.jsonl"));
            let header = serde_json::json!({"type":"session_meta", "payload":{
                "id":id,"session_id":"stale-parent","source":source,"thread_source":thread
            }});
            fs::write(&path, format!("{header}\n")).unwrap();
            symlink(path, self.0.join(format!("proc/{pid}/fd/{fd}"))).unwrap();
        }
        fn resolve(&self) -> Option<String> {
            resolve_at(100, &self.0.join("proc"), &self.0.join("sessions"))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn follows_descendants_and_ignores_subagents_and_unrelated_processes() {
        let f = Fixture::new();
        f.process(100, 1, "zsh");
        f.process(101, 100, "node");
        f.process(102, 101, "codex");
        f.process(200, 1, "codex");
        f.rollout(102, 3, "current", "cli".into(), "user");
        f.rollout(
            102,
            4,
            "child",
            serde_json::json!({"subagent":{}}),
            "subagent",
        );
        f.rollout(102, 5, "child-cli", "cli".into(), "subagent");
        f.rollout(200, 3, "unrelated", "cli".into(), "user");
        fs::write(f.0.join("proc/102/cmdline"), "codex\0resume\0old-id").unwrap();
        assert_eq!(f.resolve().as_deref(), Some("current"));
    }

    #[test]
    fn follows_rollover_of_the_same_process_without_using_other_sessions() {
        let f = Fixture::new();
        f.process(100, 1, "codex");
        f.rollout(100, 3, "old", "cli".into(), "user");
        assert_eq!(f.resolve().as_deref(), Some("old"));
        fs::remove_file(f.0.join("proc/100/fd/3")).unwrap();
        f.rollout(100, 4, "new", "cli".into(), "user");
        assert_eq!(f.resolve().as_deref(), Some("new"));
        fs::remove_file(f.0.join("proc/100/fd/4")).unwrap();
        f.rollout(
            100,
            5,
            "child",
            serde_json::json!({"subagent":{}}),
            "subagent",
        );
        assert_eq!(f.resolve(), None);
    }

    #[test]
    fn rejects_multiple_root_conversations() {
        let f = Fixture::new();
        f.process(100, 1, "codex");
        f.rollout(100, 3, "old", "cli".into(), "user");
        f.rollout(100, 4, "new", "cli".into(), "user");
        assert_eq!(f.resolve(), None);
    }

    fn stamp(f: &Fixture, id: &str, timestamp: &str) {
        let path = f.0.join(format!("sessions/rollout-{id}.jsonl"));
        let mut header: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        header["payload"]["timestamp"] = timestamp.into();
        fs::write(path, format!("{header}\n")).unwrap();
    }

    #[test]
    fn clear_selects_new_root_even_when_old_writer_remains_open_and_is_touched() {
        let f = Fixture::new();
        f.process(100, 1, "codex");
        f.rollout(100, 3, "old", "cli".into(), "user");
        f.rollout(100, 4, "new", "cli".into(), "user");
        stamp(&f, "new", "2026-09-06T03:51:50.882Z");
        stamp(&f, "old", "2026-09-06T00:12:14.738Z");
        assert_eq!(f.resolve().as_deref(), Some("new"));
        stamp(&f, "old", "2026-09-06T03:51:50.882Z");
        assert_eq!(f.resolve(), None, "equal creation times are ambiguous");
        stamp(&f, "old", "2026-09-06T00:12:14+00:00");
        assert_eq!(f.resolve(), None, "do not compare mixed timestamp formats");
    }

    #[test]
    fn newer_root_in_another_codex_process_is_not_evidence_of_clear() {
        let f = Fixture::new();
        f.process(100, 1, "zsh");
        f.process(101, 100, "codex");
        f.process(102, 100, "codex");
        f.rollout(101, 3, "old", "cli".into(), "user");
        f.rollout(102, 4, "new", "cli".into(), "user");
        stamp(&f, "old", "2026-09-06T00:12:14.738Z");
        stamp(&f, "new", "2026-09-06T03:51:50.882Z");
        assert_eq!(f.resolve(), None);
    }

    #[test]
    fn ignores_non_codex_owners() {
        let f = Fixture::new();
        f.process(100, 1, "editor");
        f.rollout(100, 3, "current", "cli".into(), "user");
        assert_eq!(f.resolve(), None);
    }

    #[test]
    fn rejects_incomplete_or_oversized_headers() {
        let f = Fixture::new();
        f.process(100, 1, "codex");
        f.rollout(100, 3, "current", "cli".into(), "user");
        let path = f.0.join("sessions/rollout-current.jsonl");
        fs::write(&path, "{\"type\":").unwrap();
        assert_eq!(f.resolve(), None);
        fs::write(path, " ".repeat(MAX_HEADER_BYTES as usize + 1)).unwrap();
        assert_eq!(f.resolve(), None);
    }
}
