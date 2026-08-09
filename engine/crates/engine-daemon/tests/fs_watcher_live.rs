//! Live `FsWatcher` behaviour against the real filesystem.
//!
//! The unit tests in `fs_watcher.rs` cover [`Debouncer`] — pure path logic that never touches
//! `notify`. Nothing exercised the watcher itself, which is how the notify 7.0 access-event change
//! reached a green CI: the daemon compiled, every test passed, and `tiderace watch` would have spun
//! forever on Linux. These tests drive the real thing.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use engine_daemon::FsWatcher;

/// Generous enough for inotify/FSEvents/ReadDirectoryChangesW to deliver, short enough to keep the
/// suite quick. The read assertion below is a *negative* — it waits out the full window every run.
const SETTLE: Duration = Duration::from_millis(400);
const DELIVER: Duration = Duration::from_millis(1200);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tiderace_fsw_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Drain whatever the initial watch registration produced, so each assertion starts from silence.
fn drain(w: &FsWatcher) {
    std::thread::sleep(SETTLE);
    while w.events().recv_timeout(Duration::from_millis(150)).is_ok() {}
}

/// Reading a watched file must NOT be reported as a change.
///
/// Regression test for the notify 7.0+ access-event feedback loop: `watch_loop` reads every path it
/// is handed in order to content-hash it, so if a read is itself reported as a change the loop feeds
/// itself indefinitely. Before the `EventKind::Access` filter this failed immediately on Linux.
#[test]
fn read_does_not_wake_the_watcher() {
    let dir = TempDir::new("read");
    let file = dir.0.join("src.py");
    fs::write(&file, b"x = 1\n").expect("seed file");

    let w = FsWatcher::watch(&dir.0).expect("watch");
    drain(&w);

    // Read repeatedly — this is exactly what watch_loop does to hash the file.
    for _ in 0..5 {
        let got = fs::read(&file).expect("read");
        assert_eq!(got, b"x = 1\n");
    }

    let spurious: Vec<_> = std::iter::from_fn(|| w.events().recv_timeout(DELIVER).ok()).collect();
    assert!(
        spurious.is_empty(),
        "reading a watched file must not report a change (got {} event(s): {:?}) — \
         this is the notify access-event feedback loop; see is_read_only_event",
        spurious.len(),
        spurious,
    );
}

/// The control: the filter must not have bought silence by dropping real changes too.
#[test]
fn write_does_wake_the_watcher() {
    let dir = TempDir::new("write");
    let file = dir.0.join("src.py");
    fs::write(&file, b"x = 1\n").expect("seed file");

    let w = FsWatcher::watch(&dir.0).expect("watch");
    drain(&w);

    fs::write(&file, b"x = 2\n").expect("modify");

    let path = w
        .events()
        .recv_timeout(DELIVER)
        .expect("a real write must be reported as a change");
    assert!(
        path.ends_with("src.py"),
        "expected the changed file, got {path:?}"
    );
}

/// A newly created file must be reported — `tiderace watch` has to notice a brand-new test module.
#[test]
fn create_does_wake_the_watcher() {
    let dir = TempDir::new("create");
    let w = FsWatcher::watch(&dir.0).expect("watch");
    drain(&w);

    fs::write(dir.0.join("test_new.py"), b"def test_a(): pass\n").expect("create");

    let path = w
        .events()
        .recv_timeout(DELIVER)
        .expect("a newly created file must be reported as a change");
    assert!(
        path.ends_with("test_new.py"),
        "expected the created file, got {path:?}"
    );
}
