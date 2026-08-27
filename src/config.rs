use crate::types::{PatternConfig, WatchDecideConfig};
use anyhow::Context;
use serde_json::Value;
use std::path::Path;

/// Deep-merges a partial override JSON on top of the shipped default. Objects
/// merge key-by-key; every other kind of value (including arrays) is replaced
/// wholesale by the override, mirroring the TS `deepMerge`.
fn deep_merge(base: Value, override_val: Value) -> Value {
    match (base, override_val) {
        (Value::Object(mut base_map), Value::Object(override_map)) => {
            for (key, over) in override_map {
                let merged = match base_map.remove(&key) {
                    Some(existing) => deep_merge(existing, over),
                    None => over,
                };
                base_map.insert(key, merged);
            }
            Value::Object(base_map)
        }
        (_, other) => other,
    }
}

fn load_json(path: &Path) -> anyhow::Result<Value> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn load_layered_value(default_path: &Path, layer_paths: &[Option<&Path>]) -> anyhow::Result<Value> {
    let mut result = load_json(default_path)?;
    for layer in layer_paths.iter().flatten() {
        if layer.exists() {
            result = deep_merge(result, load_json(layer)?);
        }
    }
    Ok(result)
}

pub fn load_config(
    default_path: &Path,
    override_path: Option<&Path>,
) -> anyhow::Result<WatchDecideConfig> {
    let merged = load_layered_value(default_path, &[override_path])?;
    Ok(serde_json::from_value(merged)?)
}

pub fn load_patterns(
    default_path: &Path,
    override_path: Option<&Path>,
) -> anyhow::Result<PatternConfig> {
    let merged = load_layered_value(default_path, &[override_path])?;
    Ok(serde_json::from_value(merged)?)
}

/// Layers pattern files on top of the shared default in order: an optional
/// agent preset (`config/presets/<agent>.patterns.json`) first, then an
/// optional per-profile override on top, which wins on any key both define.
/// Missing layer paths are skipped silently.
pub fn load_patterns_layered(
    default_path: &Path,
    layer_paths: &[Option<&Path>],
) -> anyhow::Result<PatternConfig> {
    let merged = load_layered_value(default_path, layer_paths)?;
    Ok(serde_json::from_value(merged)?)
}

/// Returns the merged `Value` for auto-respond config (caller converts to typed struct).
/// Public for use by `auto_respond.rs`.
pub fn load_auto_respond_value(
    default_path: &Path,
    override_path: Option<&Path>,
) -> anyhow::Result<serde_json::Value> {
    load_layered_value(default_path, &[override_path])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, json: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn falls_back_to_default_when_no_preset_or_override_exist() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = write(
            dir.path(),
            "default.json",
            r#"{"error":["A"],"waiting_input":[],"done":[],"working":[]}"#,
        );
        let result = load_patterns_layered(&default_path, &[None, None]).unwrap();
        assert_eq!(result.error, vec!["A".to_string()]);
    }

    #[test]
    fn agent_preset_layers_on_top_of_default() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = write(
            dir.path(),
            "default.json",
            r#"{"error":["A"],"waiting_input":["W"],"done":[],"working":[]}"#,
        );
        let preset = write(dir.path(), "codex.json", r#"{"waiting_input":["CODEX_W"]}"#);
        let result = load_patterns_layered(&default_path, &[Some(&preset), None]).unwrap();
        assert_eq!(result.error, vec!["A".to_string()]);
        assert_eq!(result.waiting_input, vec!["CODEX_W".to_string()]);
    }

    #[test]
    fn profile_override_takes_final_precedence_over_preset() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = write(
            dir.path(),
            "default.json",
            r#"{"error":["A"],"waiting_input":["W"],"done":[],"working":[]}"#,
        );
        let preset = write(dir.path(), "codex.json", r#"{"waiting_input":["CODEX_W"]}"#);
        let override_path = write(
            dir.path(),
            "profile.json",
            r#"{"waiting_input":["PROFILE_W"]}"#,
        );
        let result =
            load_patterns_layered(&default_path, &[Some(&preset), Some(&override_path)]).unwrap();
        assert_eq!(result.waiting_input, vec!["PROFILE_W".to_string()]);
    }

    #[test]
    fn skips_missing_layer_files_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = write(
            dir.path(),
            "default.json",
            r#"{"error":["A"],"waiting_input":[],"done":[],"working":[]}"#,
        );
        let missing = dir.path().join("does-not-exist.json");
        let result = load_patterns_layered(&default_path, &[Some(&missing), None]).unwrap();
        assert_eq!(result.error, vec!["A".to_string()]);
    }

    #[test]
    fn load_config_uses_default_max_capture_failures_and_allows_override() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = write(
            dir.path(),
            "default.config.json",
            r#"{
                "settleWindowMs": 4000,
                "backoff": {"workingMs": 12000, "settlingMs": 2000, "settledMs": 3000},
                "rollingContextEveryN": 5,
                "logRotation": {"maxBytes": 5242880, "maxFiles": 5},
                "safetyTimeoutMs": 1500000,
                "circuitBreaker": {"maxCrashes": 3, "windowMs": 600000},
                "telegramNotifyOnAutoImprove": true
            }"#,
        );
        let override_path = write(
            dir.path(),
            "override.config.json",
            r#"{"maxCaptureFailures": 7}"#,
        );

        let default_cfg = load_config(&default_path, None).unwrap();
        let override_cfg = load_config(&default_path, Some(&override_path)).unwrap();

        assert_eq!(default_cfg.max_capture_failures, 3);
        assert_eq!(override_cfg.max_capture_failures, 7);
    }
}
