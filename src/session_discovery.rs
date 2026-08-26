pub fn parse_tmux_list_sessions(raw: &str) -> Vec<String> {
    raw.split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Autodetecting and attaching to tmux sessions is opt-in per profile, never
/// global. Each Hermes profile only watches sessions whose name matches one of
/// its own configured prefixes — this is what keeps a "personal" profile from
/// accidentally observing a "work" session and vice versa. An empty allowlist
/// attaches to nothing rather than silently defaulting to "everything".
pub fn filter_sessions_for_profile(
    session_names: &[String],
    allowed_prefixes: &[String],
) -> Vec<String> {
    if allowed_prefixes.is_empty() {
        return Vec::new();
    }
    session_names
        .iter()
        .filter(|name| allowed_prefixes.iter().any(|p| name.starts_with(p)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn sessions() -> Vec<String> {
        strs(&[
            "work-backend",
            "prod-deploy",
            "prod-watch",
            "ariel-review",
            "personal-notes",
        ])
    }

    #[test]
    fn parses_raw_tmux_list_sessions_output_into_session_names() {
        let raw = "work-backend\nariel-review\nprod-deploy\n";
        assert_eq!(
            parse_tmux_list_sessions(raw),
            strs(&["work-backend", "ariel-review", "prod-deploy"])
        );
    }

    #[test]
    fn ignores_blank_lines() {
        assert_eq!(parse_tmux_list_sessions("a\n\nb\n\n"), strs(&["a", "b"]));
    }

    #[test]
    fn returns_empty_list_when_tmux_reports_no_sessions() {
        assert!(parse_tmux_list_sessions("").is_empty());
    }

    #[test]
    fn only_auto_attaches_sessions_matching_this_profiles_allowed_prefixes() {
        let result = filter_sessions_for_profile(&sessions(), &strs(&["prod-"]));
        assert_eq!(result, strs(&["prod-deploy", "prod-watch"]));
    }

    #[test]
    fn returns_nothing_when_no_session_matches_rather_than_all_sessions() {
        let result = filter_sessions_for_profile(&sessions(), &strs(&["nonexistent-"]));
        assert!(result.is_empty());
    }

    #[test]
    fn supports_multiple_allowed_prefixes_for_a_single_profile() {
        let result = filter_sessions_for_profile(&sessions(), &strs(&["prod-", "ariel-"]));
        assert_eq!(
            result,
            strs(&["prod-deploy", "prod-watch", "ariel-review"])
        );
    }

    #[test]
    fn with_an_empty_allowlist_attaches_nothing_explicit_opt_in_only() {
        assert!(filter_sessions_for_profile(&sessions(), &[]).is_empty());
    }
}
