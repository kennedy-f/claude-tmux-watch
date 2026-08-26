use crate::changelog::{append_changelog_entry, notify_via_hermes_send, ChangelogEntry};
use crate::circuit_breaker::CircuitBreaker;
use crate::classifier::compile_patterns;
use crate::config::{load_config, load_patterns_layered};
use crate::session_discovery::{filter_sessions_for_profile, parse_tmux_list_sessions};
use crate::tmux::{capture_pane, list_sessions};
use crate::watch_loop::{WatchLoop, WatchLoopDeps};
use clap::{Parser, Subcommand};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_HOW_TO_REPLICATE: &str = "Apply the same change to config/patterns.default.json (or the per-profile override) in every other agent using this watch/decide pattern.";

#[derive(Parser)]
#[command(
    name = "tmux-watch",
    about = "Deterministic watch/decide split for Hermes<->Claude Code tmux orchestration",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watch a tmux session and emit one JSON decision event per real decision point.
    Watch {
        #[arg(long)]
        session: String,
        #[arg(long, default_value = "default")]
        profile: String,
        /// claude-code|codex|opencode|grok|kimi
        #[arg(long)]
        agent: Option<String>,
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long)]
        once: bool,
    },
    /// List the tmux sessions belonging to a profile.
    ListSessions {
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        prefixes: Option<String>,
    },
    /// Manage the watch/decide changelog.
    Changelog {
        #[command(subcommand)]
        action: ChangelogAction,
    },
}

#[derive(Subcommand)]
enum ChangelogAction {
    Add {
        #[arg(long, default_value = "default")]
        profile: String,
        #[arg(long)]
        what: String,
        #[arg(long)]
        why: String,
        #[arg(long)]
        how: Option<String>,
        #[arg(long)]
        notify: bool,
    },
}

pub fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return err.exit_code();
        }
    };

    match cli.command {
        Command::Watch {
            session,
            profile,
            agent,
            dry_run,
            once,
        } => cmd_watch(&session, &profile, agent.as_deref(), dry_run, once),
        Command::ListSessions { profile, prefixes } => {
            cmd_list_sessions(&profile, prefixes.as_deref())
        }
        Command::Changelog {
            action:
                ChangelogAction::Add {
                    profile,
                    what,
                    why,
                    how,
                    notify,
                },
        } => cmd_changelog_add(&profile, &what, &why, how.as_deref(), notify),
    }
}

/// The directory holding `config/` — found by walking up from the executable,
/// falling back to the crate root for `cargo run` / test invocations.
fn package_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors() {
            if ancestor.join("config").join("default.config.json").is_file() {
                return ancestor.to_path_buf();
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn resolve_hermes_home() -> PathBuf {
    match std::env::var("HERMES_HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".hermes"),
    }
}

fn resolve_profile_home(profile: &str) -> PathBuf {
    let home = resolve_hermes_home();
    if profile == "default" {
        home
    } else {
        home.join("profiles").join(profile)
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn timestamp_iso() -> String {
    // RFC 3339 in UTC with millisecond precision, matching JS toISOString().
    let ms = now_ms();
    let total_secs = (ms / 1000) as i64;
    let millis = (ms % 1000) as u32;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's days-from-civil inverse.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn cmd_watch(session: &str, profile: &str, agent: Option<&str>, dry_run: bool, once: bool) -> i32 {
    let root = package_root();
    let profile_home = resolve_profile_home(profile);
    let config_override = profile_home.join("tmux-watch.config.json");
    let patterns_override = profile_home.join("tmux-watch.patterns.json");
    let agent_preset = agent.map(|a| {
        root.join("config")
            .join("presets")
            .join(format!("{a}.patterns.json"))
    });

    let config = match load_config(
        &root.join("config").join("default.config.json"),
        Some(&config_override),
    ) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("[tmux-watch] failed to load config: {err:#}");
            return 1;
        }
    };

    let pattern_source = match load_patterns_layered(
        &root.join("config").join("patterns.default.json"),
        &[agent_preset.as_deref(), Some(&patterns_override)],
    ) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("[tmux-watch] failed to load patterns: {err:#}");
            return 1;
        }
    };
    let patterns = compile_patterns(&pattern_source);

    let log_dir = profile_home.join("logs");
    if let Err(err) = ensure_dir(&log_dir) {
        eprintln!("[tmux-watch] failed to create {}: {err}", log_dir.display());
        return 1;
    }
    let log_path = log_dir.join(format!("tmux-{session}.log"));

    let mut breaker = CircuitBreaker::new(config.circuit_breaker);
    let working_backoff = Duration::from_millis(config.backoff.working_ms);

    let owned_session = session.to_string();
    let mut watch = WatchLoop::new(WatchLoopDeps {
        session: session.to_string(),
        capture_fn: move || capture_pane(&owned_session),
        patterns,
        config,
        log_path: Some(log_path.clone()),
    });

    eprintln!(
        "[tmux-watch] watching session={session} profile={profile} dryRun={dry_run} logPath={}",
        log_path.display()
    );

    // Only unexpected panics reach the circuit breaker below; expected
    // capture-pane failures are emitted as normal decision events.
    std::panic::set_hook(Box::new(|_| {}));

    loop {
        match std::panic::catch_unwind(AssertUnwindSafe(|| watch.step())) {
            Ok(result) => {
                if let Some(event) = result.event {
                    let line = serde_json::to_string(&event).unwrap_or_default();
                    if dry_run {
                        let dry_run_log = log_dir.join("dry-run-events.jsonl");
                        if let Ok(mut file) =
                            OpenOptions::new().create(true).append(true).open(&dry_run_log)
                        {
                            let _ = writeln!(file, "{line}");
                        }
                        eprintln!("[tmux-watch][dry-run] {line}");
                    } else {
                        // The decide loop (Hermes) consumes exactly one JSON line per
                        // real decision point — never a per-poll-tick stream.
                        println!("{line}");
                    }
                    if once {
                        return 0;
                    }
                }
                std::thread::sleep(Duration::from_millis(result.interval_ms));
            }
            Err(payload) => {
                let crash = breaker.record_crash(now_ms());
                eprintln!(
                    "[tmux-watch] crash ({} in window): {}",
                    crash.crashes_in_window,
                    panic_message(payload.as_ref())
                );
                if crash.tripped {
                    eprintln!(
                        "[tmux-watch] circuit breaker tripped — too many crashes, falling back to degraded mode. \
Notifying and halting the watch loop; the old per-iteration polling loop should take over."
                    );
                    notify_via_hermes_send(
                        &ChangelogEntry {
                            what: format!(
                                "tmux-watch circuit breaker tripped for session {session} (profile {profile})"
                            ),
                            why: format!(
                                "{} crashes within the configured window",
                                crash.crashes_in_window
                            ),
                            how_to_replicate:
                                "n/a — this is an incident notification, not a pattern change"
                                    .to_string(),
                        },
                        "telegram",
                    );
                    return 1;
                }
                std::thread::sleep(working_backoff);
            }
        }
    }
}

fn cmd_list_sessions(profile: &str, prefixes: Option<&str>) -> i32 {
    let prefixes: Vec<String> = match prefixes {
        Some(raw) => raw
            .split(',')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
        None => vec![format!("{profile}-")],
    };
    let all = parse_tmux_list_sessions(&list_sessions());
    for name in filter_sessions_for_profile(&all, &prefixes) {
        println!("{name}");
    }
    0
}

fn cmd_changelog_add(
    profile: &str,
    what: &str,
    why: &str,
    how: Option<&str>,
    notify: bool,
) -> i32 {
    let profile_home = resolve_profile_home(profile);
    let log_dir = profile_home.join("logs");
    if let Err(err) = ensure_dir(&log_dir) {
        eprintln!("[tmux-watch] failed to create {}: {err}", log_dir.display());
        return 1;
    }
    let changelog_path = log_dir.join("watch-decide-changelog.md");
    let entry = ChangelogEntry {
        what: what.to_string(),
        why: why.to_string(),
        how_to_replicate: how.unwrap_or(DEFAULT_HOW_TO_REPLICATE).to_string(),
    };
    if let Err(err) = append_changelog_entry(&changelog_path, &entry, &timestamp_iso()) {
        eprintln!(
            "[tmux-watch] failed to append changelog to {}: {err}",
            changelog_path.display()
        );
        return 1;
    }
    eprintln!(
        "[tmux-watch] changelog entry appended to {}",
        changelog_path.display()
    );
    if notify {
        notify_via_hermes_send(&entry, "telegram");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_resolves_to_hermes_home_itself() {
        std::env::set_var("HERMES_HOME", "/tmp/hermes-home-test");
        assert_eq!(
            resolve_profile_home("default"),
            PathBuf::from("/tmp/hermes-home-test")
        );
        assert_eq!(
            resolve_profile_home("prod"),
            PathBuf::from("/tmp/hermes-home-test/profiles/prod")
        );
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn timestamp_is_iso8601_utc() {
        let ts = timestamp_iso();
        assert!(ts.ends_with('Z'), "{ts}");
        assert_eq!(ts.len(), 24, "{ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn civil_from_days_matches_known_epoch_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }
}
