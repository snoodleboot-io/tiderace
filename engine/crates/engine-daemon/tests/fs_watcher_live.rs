//! Live `FsWatcher` behaviour against the real filesystem.
//!
//! The unit tests in `fs_watcher.rs` cover [`Debouncer`] — pure path logic that never touches
//! `notify`. Nothing exercised the watcher itself, which is how the notify 7.0 access-event change
//! reached a green CI: the daemon compiled, every test passed, and `tiderace watch` would have spun
//! forever on Linux. These tests drive the real thing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use engine_core::testing::skip_live;
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

/// Whether a watcher failed to start because the machine is out of inotify instances or watches —
/// an environmental limit, not a daemon defect (TID-69).
///
/// `inotify_init` fails with `EMFILE` once the user has `fs.inotify.max_user_instances` open, and
/// `inotify_add_watch` with `ENOSPC` past `max_user_watches`. A developer box with an IDE and a few
/// file watchers up sits at the instance limit routinely — 176 in use against 128 was the number on
/// the machine this was written on — and the three tests here reported that as three failures for
/// the whole of a benchmark stretch. Verified against clean `main` more than once; nothing in the
/// daemon was wrong. Linux only: the limit is inotify's.
fn is_inotify_exhausted(err: &notify::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        const EMFILE: i32 = 24;
        const ENOSPC: i32 = 28;
        if let notify::ErrorKind::Io(io) = &err.kind {
            return matches!(io.raw_os_error(), Some(EMFILE) | Some(ENOSPC));
        }
    }
    let _ = err;
    false
}

/// Start the watcher, or skip the test when the machine cannot give us one.
///
/// A skip, not a pass: the scenario did not run, and it says so. Under `TIDERACE_REQUIRE_LIVE=1`
/// (CI, where no runner has 170 watchers open) `skip_live` turns this into the failure it should be
/// there. Any other error is still a hard failure — the exhaustion case is the only one this excuses.
fn watch_or_skip(root: &Path) -> Option<FsWatcher> {
    match FsWatcher::watch(root) {
        Ok(w) => Some(w),
        Err(e) if is_inotify_exhausted(&e) => {
            skip_live("inotify instances exhausted (raise fs.inotify.max_user_instances)");
            None
        }
        Err(e) => panic!("watch: {e}"),
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

    let Some(w) = watch_or_skip(&dir.0) else {
        return;
    };
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

    let Some(w) = watch_or_skip(&dir.0) else {
        return;
    };
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
    let Some(w) = watch_or_skip(&dir.0) else {
        return;
    };
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

/// The classifier excuses exactly the two inotify limits and nothing else. A path that does not
/// exist is a real failure and must stay one; so is a permission error.
#[test]
fn only_inotify_exhaustion_is_excused() {
    let path_missing = notify::Error::path_not_found();
    assert!(!is_inotify_exhausted(&path_missing));
    let denied = notify::Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    assert!(!is_inotify_exhausted(&denied));

    #[cfg(target_os = "linux")]
    {
        for errno in [24, 28] {
            let e = notify::Error::io(std::io::Error::from_raw_os_error(errno));
            assert!(
                is_inotify_exhausted(&e),
                "errno {errno} is an inotify limit, not a daemon defect: {e}"
            );
        }
        let other = notify::Error::io(std::io::Error::from_raw_os_error(2)); // ENOENT
        assert!(!is_inotify_exhausted(&other));
    }
}
