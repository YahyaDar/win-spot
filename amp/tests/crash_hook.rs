//! Integration test for SpoTermAmp panic hook.

#[cfg(windows)]
#[test]
fn test_crash_hook_writes_log() {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(prefix: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("spoterm-hook-test-{}-{}-{}", prefix, std::process::id(), id));
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

    let tmp = TempDir::new("crash_hook");
    let bin_path = env!("CARGO_BIN_EXE_SpoTermAmp");

    let mut cmd = Command::new(bin_path);
    cmd.arg("--crash-test");
    cmd.env("USERPROFILE", tmp.path());

    // Remove SPOTERM_* and SPOTIPY_* variables
    for (k, _) in std::env::vars() {
        if k.starts_with("SPOTERM_") || k.starts_with("SPOTIPY_") {
            cmd.env_remove(&k);
        }
    }

    let output = cmd.output().expect("execute SpoTermAmp --crash-test");

    // Process exits with a non-zero status
    assert!(!output.status.success(), "expected non-zero exit status on panic");

    // Assert that <tmp>/.config/spoterm/crash.log exists and contains "crash test (deliberate)"
    let crash_log = tmp.path().join(".config").join("spoterm").join("crash.log");
    assert!(crash_log.is_file(), "crash.log was not created at {:?}", crash_log);

    let content = std::fs::read_to_string(&crash_log).expect("read crash.log");
    assert!(
        content.contains("crash test (deliberate)"),
        "crash.log did not contain expected panic message. Content:\n{content}"
    );
}
