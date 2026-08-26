use std::io;
use std::process::Command;

/// `-p -J`, never `-e`: plain text with wrapped lines joined back together, no
/// ANSI escape sequences. Anchored regexes (`^...`) break the moment ANSI
/// color codes are interleaved into the captured text.
pub fn build_capture_args(session: &str) -> Vec<String> {
    vec![
        "capture-pane".to_string(),
        "-p".to_string(),
        "-J".to_string(),
        "-t".to_string(),
        session.to_string(),
    ]
}

pub fn build_list_sessions_args() -> Vec<String> {
    vec![
        "list-sessions".to_string(),
        "-F".to_string(),
        "#{session_name}".to_string(),
    ]
}

/// Captures the current pane content. Uses an argv array (no shell) so a
/// session name can never be interpreted as shell syntax.
pub fn capture_pane(session: &str) -> io::Result<String> {
    let output = Command::new("tmux")
        .args(build_capture_args(session))
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "tmux capture-pane failed for session {session}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns "" when tmux exits non-zero (server not running / no sessions).
pub fn list_sessions() -> String {
    match Command::new("tmux").args(build_list_sessions_args()).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_args_use_p_and_j_and_never_e() {
        let args = build_capture_args("work-backend");
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"-J".to_string()));
        assert!(!args.contains(&"-e".to_string()));
    }

    #[test]
    fn capture_args_target_session_as_its_own_argv_entry() {
        let session = "work-backend; rm -rf /";
        let args = build_capture_args(session);
        let t_index = args.iter().position(|a| a == "-t").expect("-t present");
        assert_eq!(args[t_index + 1], session);
    }

    #[test]
    fn list_sessions_args_format_session_names_only() {
        assert_eq!(
            build_list_sessions_args(),
            vec!["list-sessions", "-F", "#{session_name}"]
        );
    }
}
