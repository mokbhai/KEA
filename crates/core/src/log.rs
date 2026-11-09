use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// The file the logger is writing to right now.
///
/// `tracing_appender::rolling::daily` never writes a bare `kea.log`: it
/// appends the date, so today's file is `kea.log.YYYY-MM-DD`. The newest of
/// those is the live one. ISO dates sort lexically, so the greatest name is
/// the newest. A bare `kea.log` is only the fallback for a directory the
/// roller has not written to yet.
pub fn current_log_path(log_dir: &Path) -> PathBuf {
    let newest = std::fs::read_dir(log_dir).ok().and_then(|dir| {
        dir.flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| is_rotated_log_name(name))
            .max()
    });
    match newest {
        Some(name) => log_dir.join(name),
        None => log_dir.join("kea.log"),
    }
}

fn is_rotated_log_name(name: &str) -> bool {
    let Some(date) = name.strip_prefix("kea.log.") else {
        return false;
    };
    date.len() == 10
        && date
            .chars()
            .enumerate()
            .all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() })
}

/// Deletes rotated log files older than `days` days. The rolling appender
/// creates files named `kea.log.YYYY-MM-DD`; this prunes those by parsing
/// the date suffix and comparing against the cutoff.
pub fn prune_old_logs(log_dir: &Path, days: u64) -> Result<u64, std::io::Error> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(days * 86400);

    let mut pruned = 0u64;
    let dir = match std::fs::read_dir(log_dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };

    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("kea.log.") {
            continue;
        }
        let date_part = &name["kea.log.".len()..];
        if date_part.len() != 10 {
            continue;
        }
        // The rolling appender uses YYYY-MM-DD.
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            if let Ok(secs) = modified.duration_since(std::time::UNIX_EPOCH) {
                if secs.as_secs() < cutoff {
                    std::fs::remove_file(entry.path())?;
                    pruned += 1;
                }
            }
        }
    }
    Ok(pruned)
}

pub fn tail_log_file(path: &Path, max_bytes: usize) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        return Ok(String::new());
    }
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start as u64))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn init_logging(log_dir: &Path, level: &str) -> WorkerGuard {
    let file_appender = tracing_appender::rolling::daily(log_dir, "kea.log");
    let (nb, guard) = tracing_appender::non_blocking(file_appender);
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(nb).with_ansi(false))
        .try_init();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::info;

    #[test]
    fn writes_a_log_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _guard = init_logging(dir.path(), "info");
            info!("hello-kea");
        } // guard drop flushes
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(!files.is_empty(), "expected a log file to be created");
    }

    #[test]
    fn current_log_path_falls_back_to_kea_log_in_an_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(current_log_path(dir.path()), dir.path().join("kea.log"));
    }

    /// The Logs page showed "(Log file is empty or not created yet.)" while
    /// the folder held weeks of `kea.log.YYYY-MM-DD` files: the reader looked
    /// for a bare `kea.log` the daily roller never creates.
    #[test]
    fn current_log_path_picks_the_newest_rotated_file() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "kea.log.2026-09-30",
            "kea.log.2026-11-09",
            "kea.log.2026-10-31",
            "kea.log.bak",
            "other.log.2026-12-31",
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        assert_eq!(
            current_log_path(dir.path()),
            dir.path().join("kea.log.2026-11-09")
        );
    }

    #[test]
    fn current_log_path_finds_what_the_daily_roller_writes() {
        use std::io::Write;
        // The roller directly, not `init_logging`: the global subscriber is
        // process-wide and another test may already have installed it.
        let dir = tempfile::tempdir().unwrap();
        let mut roller = tracing_appender::rolling::daily(dir.path(), "kea.log");
        roller.write_all(b"hello-from-the-roller\n").unwrap();
        roller.flush().unwrap();
        let path = current_log_path(dir.path());
        let tail = tail_log_file(&path, 4096).unwrap();
        assert!(tail.contains("hello-from-the-roller"), "read {path:?}: {tail:?}");
    }

    #[test]
    fn tail_log_file_returns_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kea.log");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let tail = tail_log_file(&path, 12).unwrap();
        assert!(tail.contains("line3"));
        assert!(!tail.contains("line1"));
    }

    #[test]
    fn tail_log_file_returns_full_when_smaller_than_max() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kea.log");
        let content = "short log";
        std::fs::write(&path, content).unwrap();
        let tail = tail_log_file(&path, 1024).unwrap();
        assert_eq!(tail, content);
    }
}
