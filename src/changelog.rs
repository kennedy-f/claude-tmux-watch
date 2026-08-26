use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

pub struct ChangelogEntry {
    pub what: String,
    pub why: String,
    pub how_to_replicate: String,
}

/// Every self-improvement the watch/decide loop makes to its own patterns or
/// thresholds must be traceable: what changed, the real failure that motivated
/// it, and how to carry the same fix to another agent using this orchestration
/// pattern (Codex, OpenCode, ...).
pub fn format_changelog_entry(entry: &ChangelogEntry, timestamp_iso: &str) -> String {
    [
        format!("## {timestamp_iso}"),
        String::new(),
        format!("**What:** {}", entry.what),
        format!("**Why:** {}", entry.why),
        format!("**How to replicate elsewhere:** {}", entry.how_to_replicate),
        String::new(),
        "---".to_string(),
        String::new(),
    ]
    .join("\n")
}

pub fn append_changelog_entry(
    path: &Path,
    entry: &ChangelogEntry,
    timestamp_iso: &str,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format_changelog_entry(entry, timestamp_iso).as_bytes())
}

pub fn build_notify_message(entry: &ChangelogEntry) -> String {
    format!(
        "[watch/decide auto-improve] {}\nWhy: {}",
        entry.what, entry.why
    )
}

/// Sends the notification via the existing `hermes send` CLI, which reuses
/// whatever platform the active profile is already configured for. Arguments go
/// through an argv array, never a shell string. Failures are logged to stderr
/// but never crash the watch loop — a missed notification is not worth taking
/// the loop down over.
pub fn notify_via_hermes_send(entry: &ChangelogEntry, target: &str) {
    let message = build_notify_message(entry);
    let output = Command::new("hermes")
        .args(["send", "--to", target, &message])
        .output();

    match output {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            eprintln!(
                "[tmux-watch] failed to notify via 'hermes send --to {target}': {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Err(err) => {
            eprintln!("[tmux-watch] failed to notify via 'hermes send --to {target}': {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry() -> ChangelogEntry {
        ChangelogEntry {
            what: "Added pattern for Codex-style '? for suggestions' waiting_input prompt"
                .to_string(),
            why: "3 false 'working' classifications observed in prod session prod-deploy on 2026-08-26 — the loop never fired a decision event for ~40min".to_string(),
            how_to_replicate: "Add the same regex to config/patterns.default.json under waiting_input in any other agent using the watch/decide pattern (e.g. Codex, OpenCode orchestration skills)".to_string(),
        }
    }

    #[test]
    fn format_includes_what_why_how_to_replicate_and_timestamp() {
        let e = entry();
        let md = format_changelog_entry(&e, "2026-08-26T12:00:00.000Z");
        assert!(md.contains("2026-08-26T12:00:00.000Z"));
        assert!(md.contains(&e.what));
        assert!(md.contains(&e.why));
        assert!(md.contains(&e.how_to_replicate));
    }

    #[test]
    fn creates_the_changelog_file_on_first_entry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("watch-decide-changelog.md");
        let e = entry();
        append_changelog_entry(&path, &e, "2026-08-26T12:00:00.000Z").unwrap();
        assert!(path.exists());
        assert!(fs::read_to_string(&path).unwrap().contains(&e.what));
    }

    #[test]
    fn appends_subsequent_entries_instead_of_overwriting() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("watch-decide-changelog.md");
        let e = entry();
        append_changelog_entry(&path, &e, "2026-08-26T12:00:00.000Z").unwrap();
        let second = ChangelogEntry {
            what: "second change".to_string(),
            why: e.why.clone(),
            how_to_replicate: e.how_to_replicate.clone(),
        };
        append_changelog_entry(&path, &second, "2026-08-26T13:00:00.000Z").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(&e.what));
        assert!(content.contains("second change"));
    }

    #[test]
    fn build_notify_message_produces_short_human_readable_message() {
        let e = entry();
        let msg = build_notify_message(&e);
        assert!(msg.contains("watch/decide"));
        assert!(msg.contains(&e.what));
    }
}
