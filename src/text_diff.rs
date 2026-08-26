use crate::types::DeltaResult;
use sha2::{Digest, Sha256};

pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Diffs two pane-content snapshots by line content, never by numeric offset.
/// tmux's history-limit has a ceiling: once hit, old lines fall out of the
/// buffer and every remaining line shifts up, which would corrupt any diff
/// keyed on a saved line index. Instead this finds the longest overlap where a
/// suffix of `prev` equals a prefix of `next`.
pub fn diff_lines(prev_content: &str, next_content: &str) -> DeltaResult {
    let prev_lines: Vec<&str> = if prev_content.is_empty() {
        Vec::new()
    } else {
        prev_content.split('\n').collect()
    };
    let next_lines: Vec<&str> = if next_content.is_empty() {
        Vec::new()
    } else {
        next_content.split('\n').collect()
    };

    let max_k = prev_lines.len().min(next_lines.len());
    let mut best_k = 0;
    for k in (0..=max_k).rev() {
        if prev_lines[prev_lines.len() - k..] == next_lines[..k] {
            best_k = k;
            break;
        }
    }

    let added_lines: Vec<String> = next_lines[best_k..].iter().map(|s| s.to_string()).collect();
    let changed = !added_lines.is_empty();
    DeltaResult {
        added_lines,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_for_identical_content() {
        assert_eq!(hash_content("abc\ndef"), hash_content("abc\ndef"));
    }

    #[test]
    fn hash_changes_when_content_changes() {
        assert_ne!(hash_content("abc"), hash_content("abcd"));
    }

    #[test]
    fn returns_only_newly_appended_lines_not_the_whole_buffer() {
        let delta = diff_lines("line1\nline2\nline3", "line1\nline2\nline3\nline4\nline5");
        assert!(delta.changed);
        assert_eq!(delta.added_lines, vec!["line4", "line5"]);
    }

    /// tmux history-limit eviction shifts every line up; a line-offset diff
    /// would misread this as a huge new delta.
    #[test]
    fn handles_scrollback_eviction_shifting_content() {
        let delta = diff_lines("lineA\nlineB\nlineC\nlineD", "lineB\nlineC\nlineD\nlineE");
        assert_eq!(delta.added_lines, vec!["lineE"]);
    }

    #[test]
    fn reports_no_change_when_content_is_identical() {
        let delta = diff_lines("same\ntext", "same\ntext");
        assert!(!delta.changed);
        assert!(delta.added_lines.is_empty());
    }

    #[test]
    fn falls_back_to_full_next_content_when_there_is_no_overlap() {
        let delta = diff_lines("totally\nunrelated", "brand\nnew\ncontent");
        assert!(delta.changed);
        assert_eq!(delta.added_lines, vec!["brand", "new", "content"]);
    }

    #[test]
    fn treats_an_empty_previous_buffer_as_all_new_content() {
        let delta = diff_lines("", "first\nsecond");
        assert_eq!(delta.added_lines, vec!["first", "second"]);
    }
}
