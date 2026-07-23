use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cache::{Cache, CacheKey, CachedOutcome};

/// A **directory-backed** cache tier — the shareable remote store (ADR-E004). Each entry is a JSON file
/// named by the content hash (`<hex>.json`) in `root`, so the store is just a directory: point it at a
/// CI cache path (`actions/cache`), a shared mount / NFS, or an artifact dir and a green result computed
/// on one machine is a free hit on any other with the same inputs.
///
/// It sits behind the [`Cache`] seam exactly like [`LocalCache`](crate::cache::LocalCache), so
/// [`TieredCache`](crate::cache::TieredCache)`::with_remote(local, DirCache::new(dir))` gives the
/// local→remote→backfill flow with zero orchestrator changes. A future HTTP/object-store client is a
/// drop-in behind the same trait.
///
/// **Best-effort by design**: a cache is an optimization, so every I/O error is a silent miss / no-op —
/// `get`/`put` never panic and never fail a run. Writes are atomic (temp file + rename) so a concurrent
/// reader (or a killed writer) never observes a half-written entry.
#[derive(Debug, Clone)]
pub struct DirCache {
    root: PathBuf,
}

/// Process-global counter making each in-flight temp file name unique (multiple threads may write the
/// same key concurrently; they must not collide on the temp path before the atomic rename).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl DirCache {
    /// Open (and best-effort create) the cache directory at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = std::fs::create_dir_all(&root); // get/put tolerate a missing dir anyway
        Self { root }
    }

    /// The on-disk path for a key's entry.
    fn entry_path(&self, key: &CacheKey) -> PathBuf {
        self.root.join(format!("{}.json", key.to_hex()))
    }

    /// Number of stored entries (`*.json` files) — diagnostics/tests.
    pub fn len(&self) -> usize {
        std::fs::read_dir(&self.root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Whether the store has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The directory this cache reads/writes (for diagnostics).
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Cache for DirCache {
    fn get(&self, key: &CacheKey) -> Option<CachedOutcome> {
        // A missing file, an unreadable one, or a corrupt/foreign entry all read as a plain miss.
        let bytes = std::fs::read(self.entry_path(key)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn put(&self, key: &CacheKey, outcome: CachedOutcome) {
        let Ok(json) = serde_json::to_vec(&outcome) else {
            return; // unserializable ⇒ just don't cache
        };
        // Write to a unique temp file in the same directory, then atomically rename into place, so a
        // reader never sees a partial entry and two writers of the same key can't corrupt each other.
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(
            ".{}.{}.{seq}.tmp",
            key.to_hex(),
            std::process::id()
        ));
        if std::fs::write(&tmp, &json).is_ok() {
            if std::fs::rename(&tmp, self.entry_path(key)).is_err() {
                let _ = std::fs::remove_file(&tmp); // rename failed (e.g. dir vanished) → clean up
            }
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheKeyBuilder, LocalCache, TieredCache};
    use crate::domain::Outcome;

    fn key(node: &str) -> CacheKey {
        CacheKeyBuilder::new(node, "0.5.0", "3.12", "linux").finish()
    }

    /// A unique temp directory per test (removed on drop).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "tiderace_dircache_{tag}_{}_{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn miss_then_hit_on_disk() {
        let dir = TempDir::new("hit");
        let c = DirCache::new(&dir.0);
        let k = key("t.py::a");
        assert!(c.get(&k).is_none());
        c.put(&k, CachedOutcome::new(Outcome::Passed, "ok"));
        let hit = c.get(&k).expect("hit after put");
        assert_eq!(hit.outcome(), Outcome::Passed);
        assert_eq!(hit.detail(), "ok");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn persists_across_instances_same_dir() {
        // The "share across machines" property: machine A writes; a fresh cache over the same directory
        // (machine B, having pulled the CI artifact) reads a hit.
        let dir = TempDir::new("share");
        let k = key("t.py::shared");
        DirCache::new(&dir.0).put(&k, CachedOutcome::new(Outcome::Failed, "boom"));

        let machine_b = DirCache::new(&dir.0); // separate instance, same directory
        let hit = machine_b.get(&k).expect("cross-instance hit");
        assert_eq!(hit.outcome(), Outcome::Failed);
        assert_eq!(hit.detail(), "boom");
    }

    #[test]
    fn distinct_keys_do_not_collide() {
        let dir = TempDir::new("distinct");
        let c = DirCache::new(&dir.0);
        c.put(&key("t.py::a"), CachedOutcome::new(Outcome::Passed, ""));
        c.put(&key("t.py::b"), CachedOutcome::new(Outcome::Failed, "x"));
        assert_eq!(c.get(&key("t.py::a")).unwrap().outcome(), Outcome::Passed);
        assert_eq!(c.get(&key("t.py::b")).unwrap().outcome(), Outcome::Failed);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn corrupt_or_foreign_entry_is_a_miss_not_a_panic() {
        let dir = TempDir::new("corrupt");
        let c = DirCache::new(&dir.0);
        let k = key("t.py::a");
        std::fs::write(c.entry_path(&k), b"{ not valid json").unwrap();
        assert!(c.get(&k).is_none(), "corrupt entry reads as a miss");
    }

    #[test]
    fn get_on_missing_directory_is_a_miss() {
        let c = DirCache::new("/nonexistent/tiderace/cache/dir");
        assert!(c.get(&key("t.py::a")).is_none());
        // put must not panic even if the directory can't be created/written.
        c.put(&key("t.py::a"), CachedOutcome::new(Outcome::Passed, ""));
    }

    #[test]
    fn tiered_with_dir_remote_backfills_local() {
        // CI populated the shared dir; a fresh machine's local tier is cold → remote hit → backfill.
        let dir = TempDir::new("tiered");
        let k = key("t.py::a");
        DirCache::new(&dir.0).put(&k, CachedOutcome::new(Outcome::Passed, "")); // "CI" wrote it

        let tiered = TieredCache::with_remote(LocalCache::new(), DirCache::new(&dir.0));
        assert_eq!(tiered.get(&k).unwrap().outcome(), Outcome::Passed); // local miss → remote hit
    }
}
