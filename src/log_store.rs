use crate::types::LogRotationConfig;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Raw pane output is persisted here in full, outside of whatever slice is sent
/// to the LLM. A VPS previously hit a disk/RAM incident from an unbounded log,
/// so rotation and retention are not optional.
pub fn append_with_rotation(
    log_path: &Path,
    content: &str,
    config: &LogRotationConfig,
) -> std::io::Result<()> {
    let current_size = fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);
    let content_bytes = content.as_bytes().len() as u64;

    if current_size > 0 && current_size + content_bytes > config.max_bytes {
        rotate(log_path, config.max_files)?;
        fs::write(log_path, content)?;
        return Ok(());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    file.write_all(content.as_bytes())
}

fn numbered(log_path: &Path, n: u32) -> PathBuf {
    let mut name = log_path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

fn rotate(log_path: &Path, max_files: u32) -> std::io::Result<()> {
    let oldest = numbered(log_path, max_files);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for i in (1..max_files).rev() {
        let src = numbered(log_path, i);
        let dst = numbered(log_path, i + 1);
        if src.exists() {
            fs::rename(&src, &dst)?;
        }
    }
    if log_path.exists() {
        fs::rename(log_path, numbered(log_path, 1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("tmux-work-backend.log");
        (dir, log_path)
    }

    #[test]
    fn appends_to_the_log_file_creating_it_if_missing() {
        let (_dir, log_path) = setup();
        let config = LogRotationConfig {
            max_bytes: 1024,
            max_files: 3,
        };
        append_with_rotation(&log_path, "line one\n", &config).unwrap();
        append_with_rotation(&log_path, "line two\n", &config).unwrap();
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            "line one\nline two\n"
        );
    }

    #[test]
    fn rotates_the_file_once_it_exceeds_max_bytes() {
        let (_dir, log_path) = setup();
        fs::write(&log_path, "x".repeat(100)).unwrap();
        append_with_rotation(
            &log_path,
            &"y".repeat(50),
            &LogRotationConfig {
                max_bytes: 100,
                max_files: 3,
            },
        )
        .unwrap();
        assert!(numbered(&log_path, 1).exists());
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "y".repeat(50));
    }

    #[test]
    fn caps_retained_rotated_files_at_max_files_deleting_the_oldest() {
        let (_dir, log_path) = setup();
        let config = LogRotationConfig {
            max_bytes: 100,
            max_files: 2,
        };
        fs::write(&log_path, "a".repeat(100)).unwrap();
        append_with_rotation(&log_path, &"b".repeat(50), &config).unwrap();
        append_with_rotation(&log_path, &"c".repeat(101), &config).unwrap();
        append_with_rotation(&log_path, &"d".repeat(101), &config).unwrap();

        assert!(numbered(&log_path, 1).exists());
        assert!(numbered(&log_path, 2).exists());
        assert!(!numbered(&log_path, 3).exists());
        // .1 is the newest rotated content, .2 the older one.
        assert_eq!(
            fs::read_to_string(numbered(&log_path, 1)).unwrap(),
            "c".repeat(101)
        );
        assert_eq!(
            fs::read_to_string(numbered(&log_path, 2)).unwrap(),
            "b".repeat(50)
        );
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "d".repeat(101));
    }
}
