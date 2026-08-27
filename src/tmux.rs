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

/// Builds the argv array for `tmux send-keys`. Arguments are passed through
/// an argv array so a session name can never be interpreted as shell syntax.
pub fn build_send_keys_args(session: &str, keys: &[String]) -> Vec<String> {
    let mut args = vec![
        "send-keys".to_string(),
        "-t".to_string(),
        session.to_string(),
    ];
    args.extend_from_slice(keys);
    args
}

/// Sends key strokes to the target tmux pane. Uses an argv array (no shell).
pub fn send_keys(session: &str, keys: &[String]) -> io::Result<()> {
    let output = Command::new("tmux")
        .args(build_send_keys_args(session, keys))
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "tmux send-keys failed for {session}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Returns "" when tmux exits non-zero (server not running / no sessions).
pub fn list_sessions() -> String {
    match Command::new("tmux")
        .args(build_list_sessions_args())
        .output()
    {
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
    fn send_keys_args_never_use_shell_expansion() {
        let session = "work-backend; rm -rf /";
        let keys = vec!["1".to_string(), "Enter".to_string()];
        let args = build_send_keys_args(session, &keys);
        let t_index = args.iter().position(|a| a == "-t").expect("-t present");
        assert_eq!(args[t_index + 1], session);
        assert!(args.contains(&"1".to_string()));
        assert!(args.contains(&"Enter".to_string()));
    }

    #[test]
    fn send_keys_args_start_with_send_keys() {
        let args = build_send_keys_args("s", &["y".to_string()]);
        assert_eq!(args[0], "send-keys");
    }

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
