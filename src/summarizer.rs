use crate::types::PaneState;
use regex::Regex;
use std::sync::OnceLock;

fn file_ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[\w./-]+\.(?:ts|tsx|js|jsx|py|md|json|ya?ml)\b").unwrap())
}

fn pr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#(\d+)").unwrap())
}

fn command_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^[ \t]*[❯$]\s+(.+?)\s*$").unwrap())
}

fn state_suffix(state: PaneState) -> &'static str {
    match state {
        PaneState::WaitingInput => "Aguardando confirmação.",
        PaneState::Done => "Concluído.",
        PaneState::Error => "Erro reportado.",
        PaneState::Working => "Em andamento.",
    }
}

fn unique_matches(re: &Regex, text: &str, group: usize) -> Vec<String> {
    let mut seen = Vec::new();
    for caps in re.captures_iter(text) {
        let Some(m) = caps.get(group) else { continue };
        let value = m.as_str().to_string();
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen
}

pub fn extract_pr_numbers(text: &str) -> Vec<String> {
    unique_matches(pr_re(), text, 1)
}

pub fn extract_file_paths(text: &str) -> Vec<String> {
    unique_matches(file_ext_re(), text, 0)
}

pub fn extract_commands(text: &str) -> Vec<String> {
    unique_matches(command_line_re(), text, 1)
}

/// Builds a compact rolling-context summary purely by regex/heuristic extraction
/// over the raw delta text — no LLM call. Summarizing with an LLM would
/// reintroduce exactly the per-iteration cost this watch/decide split exists to
/// eliminate.
pub fn summarize(text: &str, state: PaneState) -> String {
    let prs = extract_pr_numbers(text);
    let files = extract_file_paths(text);
    let commands = extract_commands(text);

    let mut parts: Vec<String> = Vec::new();
    if !prs.is_empty() {
        let plural = if prs.len() > 1 { "s" } else { "" };
        let list = prs
            .iter()
            .map(|p| format!("#{p}"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("Revisou PR{plural} {list}."));
    }
    if !files.is_empty() {
        parts.push(format!("Arquivos: {}.", files.join(", ")));
    }
    if !commands.is_empty() {
        parts.push(format!("Comandos: {}.", commands.join(", ")));
    }
    parts.push(state_suffix(state).to_string());

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_pr_numbers_referenced_with_hash() {
        let text = "Reviewed #1237 and #1239, then re-checked #1237 again.";
        assert_eq!(extract_pr_numbers(text), vec!["1237", "1239"]);
    }

    #[test]
    fn extracts_file_paths_with_known_source_extensions() {
        let text = "Edited src/app.ts and docs/README.md, skipped node_modules noise.";
        assert_eq!(
            extract_file_paths(text),
            vec!["src/app.ts", "docs/README.md"]
        );
    }

    #[test]
    fn extracts_shell_commands_from_prompt_prefixed_lines() {
        let text = "❯ npm test\nsome output\n$ git status\nmore output";
        assert_eq!(extract_commands(text), vec!["npm test", "git status"]);
    }

    #[test]
    fn mentions_reviewed_prs_and_the_waiting_input_state() {
        let text = "Revisando #1237 e #1239 antes de seguir.";
        let summary = summarize(text, PaneState::WaitingInput);
        assert!(summary.contains("#1237"));
        assert!(summary.contains("#1239"));
        assert!(summary.to_lowercase().contains("aguardando"));
    }

    #[test]
    fn produces_a_plain_state_note_when_nothing_structured_to_extract() {
        let summary = summarize("just some prose with nothing special", PaneState::Done);
        assert!(summary.to_lowercase().contains("conclu"));
    }

    #[test]
    fn is_a_pure_function_of_its_inputs_never_touches_network_or_llm() {
        let text = "Ran $ pytest -q on src/app.py, waiting on #42.";
        assert_eq!(
            summarize(text, PaneState::WaitingInput),
            summarize(text, PaneState::WaitingInput)
        );
    }
}
