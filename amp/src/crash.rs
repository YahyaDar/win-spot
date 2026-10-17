//! Writes panic reports to <config>/crash.log (SpoTermAmp has no console).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

const MAX_LOG_SIZE: u64 = 256 * 1024; // 256 KiB

/// Pure formatting, easy to test.
pub fn format_report(
    version: &str,
    unix_secs: u64,
    thread: &str,
    message: &str,
    location: Option<&str>,
    backtrace: &str,
) -> String {
    let loc = location.unwrap_or("<unknown location>");
    format!(
        "=== SpoTermAmp Crash Report ===\n\
         Version:   {version}\n\
         Time:      {unix_secs}\n\
         Thread:    {thread}\n\
         Location:  {loc}\n\
         Message:   {message}\n\
         \n\
         --- Backtrace ---\n\
         {backtrace}\n\
         ================================\n\n"
    )
}

/// Append a report to `<dir>/crash.log`. If the file is already over 256 KiB, first
/// rename it to `crash.log.1` (replacing an older one). Creates `dir` if needed.
/// Never panics; returns the I/O error instead.
pub fn append(dir: &Path, report: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let log_path = dir.join("crash.log");
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > MAX_LOG_SIZE {
            let backup_path = dir.join("crash.log.1");
            let _ = fs::remove_file(&backup_path);
            let _ = fs::rename(&log_path, &backup_path);
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    file.write_all(report.as_bytes())?;
    file.flush()
}

/// Install the hook. Keeps and calls the previous hook afterwards.
pub fn install() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "<non-string panic>"
        };

        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let thread_name = std::thread::current();
        let thread = thread_name.name().unwrap_or("<unnamed>");
        let unix_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let version = env!("CARGO_PKG_VERSION");

        let report = format_report(
            version,
            unix_secs,
            thread,
            msg,
            location.as_deref(),
            &backtrace,
        );

        let _ = append(&crate::config::config_dir(), &report);

        prev_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(prefix: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("spoterm-crash-test-{}-{}-{}", prefix, std::process::id(), id));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_format_report() {
        let multiline_msg = "line1\nline2\nline3";
        let report = format_report(
            "1.2.3",
            1700000000,
            "worker-thread",
            multiline_msg,
            Some("src/app.rs:42:10"),
            "frame 0: foo\nframe 1: bar",
        );
        assert!(report.contains("Version:   1.2.3"));
        assert!(report.contains("Time:      1700000000"));
        assert!(report.contains("Thread:    worker-thread"));
        assert!(report.contains("Location:  src/app.rs:42:10"));
        assert!(report.contains(multiline_msg));
        assert!(report.contains("Backtrace"));
        assert!(report.contains("frame 0: foo"));

        // location: None
        let report_noloc = format_report(
            "1.2.3",
            1700000000,
            "main",
            "panic message",
            None,
            "trace",
        );
        assert!(report_noloc.contains("Location:  <unknown location>"));
        assert!(report_noloc.contains("panic message"));
    }

    #[test]
    fn test_append_fresh_dir() {
        let tmp = TempDir::new("fresh");
        let sub = tmp.path().join("nested").join("folder");
        let report = "test report 1\n";
        assert!(append(&sub, report).is_ok());

        let log = sub.join("crash.log");
        assert!(log.is_file());
        assert_eq!(fs::read_to_string(&log).unwrap(), report);
    }

    #[test]
    fn test_append_two_reports() {
        let tmp = TempDir::new("two");
        let report1 = "report 1\n";
        let report2 = "report 2\n";
        assert!(append(tmp.path(), report1).is_ok());
        assert!(append(tmp.path(), report2).is_ok());

        let log = tmp.path().join("crash.log");
        let content = fs::read_to_string(&log).unwrap();
        assert_eq!(content, format!("{report1}{report2}"));
    }

    #[test]
    fn test_append_rotation() {
        let tmp = TempDir::new("rotation");
        let log = tmp.path().join("crash.log");
        let backup = tmp.path().join("crash.log.1");

        // Pre-existing crash.log.1
        fs::write(&backup, "old pre-existing backup").unwrap();

        // 300 KiB crash.log (over 256 KiB)
        let large_content = "x".repeat(300 * 1024);
        fs::write(&log, &large_content).unwrap();

        // Append new report
        let new_report = "fresh crash report\n";
        assert!(append(tmp.path(), new_report).is_ok());

        // crash.log.1 contains the 300 KiB content (replacing pre-existing backup)
        assert!(backup.is_file());
        assert_eq!(fs::read_to_string(&backup).unwrap(), large_content);

        // crash.log has only the new report
        assert_eq!(fs::read_to_string(&log).unwrap(), new_report);
    }

    #[test]
    fn test_append_dir_is_file() {
        let tmp = TempDir::new("dir_is_file");
        let file_path = tmp.path().join("not_a_dir");
        fs::write(&file_path, "regular file content").unwrap();

        // Calling append where dir is actually a file returns Err and does not panic
        let res = append(&file_path, "report");
        assert!(res.is_err());
    }
}
