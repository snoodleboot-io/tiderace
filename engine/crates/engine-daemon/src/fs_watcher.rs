use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Coalesces a burst of filesystem events into a deduplicated batch (design 08 `fs_watcher.rs`). A
/// single save often emits several events for one file (and editors touch temp/backup files); the
/// debouncer collapses them so the daemon classifies each real file once per quiet window.
#[derive(Debug, Default)]
pub struct Debouncer {
    pending: BTreeSet<PathBuf>,
}

impl Debouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a changed path (ignores editor temp/backup noise).
    pub fn record(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if is_noise(&path) {
            return;
        }
        self.pending.insert(path);
    }

    /// Drain the coalesced batch (sorted, each path once).
    pub fn take(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

fn is_noise(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.is_empty()
        || name.ends_with('~') // editor backups
        || name.ends_with(".swp") // vim
        || name.starts_with(".#") // emacs lockfiles
        || name.starts_with("__pycache__")
        || path.components().any(|c| c.as_os_str() == "__pycache__" || c.as_os_str() == ".git")
}

/// Is this event a pure *read* of the file rather than a change to it?
///
/// This exists to break a self-sustaining feedback loop. From notify 7.0 the Linux/inotify backend
/// reports access-open events, so merely *reading* a watched file emits an event — and the watch
/// loop reads every path it is handed in order to content-hash it ([`watch_loop`](crate::watch_loop)).
/// Without this filter that read emits the next event, which wakes the loop, which reads again:
/// one stray read anywhere under the root spins forever at ~20 events/sec, printing `Idle` each
/// time. Measured before the filter: a single read produced 118 lines in 6s and was still climbing;
/// on notify 6.1.1 (which never emitted the access event) it produced none.
///
/// Access events carry no information the daemon can act on — the content cannot have changed — so
/// dropping them costs nothing and restores the pre-7.0 semantics the watch loop was written against.
fn is_read_only_event(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Access(_))
}

/// A live filesystem watcher over `notify`: recursively watches `root` and forwards each changed path.
/// The caller drains via a [`Debouncer`] on a quiet-window timer (the time policy lives in the daemon
/// loop, kept out of this thin wrapper so the coalescing logic stays unit-testable).
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    events: Receiver<PathBuf>,
}

impl FsWatcher {
    /// Begin watching `root` recursively. Returns the watcher (keep it alive) + a path receiver.
    pub fn watch(root: &Path) -> notify::Result<Self> {
        let (tx, rx) = channel::<PathBuf>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    if is_read_only_event(&event.kind) {
                        return;
                    }
                    for path in event.paths {
                        let _ = tx.send(path); // receiver gone ⇒ daemon shutting down; drop silently
                    }
                }
            })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events: rx,
        })
    }

    /// The receiver of changed paths (drain into a `Debouncer`).
    pub fn events(&self) -> &Receiver<PathBuf> {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_duplicate_events() {
        let mut d = Debouncer::new();
        d.record("src/a.py");
        d.record("src/a.py"); // duplicate from the same save
        d.record("src/b.py");
        assert_eq!(
            d.take(),
            vec![PathBuf::from("src/a.py"), PathBuf::from("src/b.py")]
        );
        assert!(d.is_empty(), "take() drains the batch");
    }

    #[test]
    fn only_access_events_count_as_read_only() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

        // Dropped: a read tells the daemon nothing and would feed the watch loop back into itself.
        assert!(is_read_only_event(&EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Read
        ))));
        assert!(is_read_only_event(&EventKind::Access(AccessKind::Any)));

        // Kept: every kind that can actually change bytes on disk.
        assert!(!is_read_only_event(&EventKind::Modify(ModifyKind::Any)));
        assert!(!is_read_only_event(&EventKind::Create(CreateKind::File)));
        assert!(!is_read_only_event(&EventKind::Remove(RemoveKind::File)));
        // `Any`/`Other` are unknown-to-us kinds — keep them; a missed re-run is worse than a
        // redundant hash that resolves to Idle.
        assert!(!is_read_only_event(&EventKind::Any));
        assert!(!is_read_only_event(&EventKind::Other));
    }

    #[test]
    fn filters_editor_and_cache_noise() {
        let mut d = Debouncer::new();
        d.record("src/a.py~");
        d.record("src/.#a.py");
        d.record("src/a.py.swp");
        d.record("src/__pycache__/a.cpython-312.pyc");
        d.record(".git/index");
        assert!(d.take().is_empty(), "all noise must be filtered");
    }
}
