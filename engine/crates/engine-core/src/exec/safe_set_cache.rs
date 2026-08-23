//! Content-keyed cache of sub-interpreter safety verdicts, shared by both front ends (TID-35).
//!
//! Probing a module means launching a fresh interpreter and trying to import it there, which is the
//! only way to learn whether it can load in a sub-interpreter at all — a single-phase C extension
//! (numpy is the canonical case) cannot. That is expensive, and the answer only changes when the
//! module's source does, so it is exactly the shape of thing to cache by content hash.
//!
//! The cache used to live inside the daemon's persisted state (TID-9), which meant `tiderace run
//! --strategy subinterp` paid a full probe pass on **every** invocation. On the 20-module corpus
//! that tier was measured against, probing is most of its fixed cost — so the tier's published
//! numbers were partly measuring the probe. It also lands hardest in the one place the tier exists
//! for: Windows, where there is no fork, no daemon assumption, and no alternative to falling back
//! to sequential.
//!
//! Verdicts are keyed by the module's content hash, so a changed module re-probes. Undeterminable
//! modules — CPython < 3.14, no probe API — are deliberately **not** cached: `None` means "we could
//! not tell", and caching that would freeze a temporary environment fact into a persistent file that
//! outlives an interpreter upgrade.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exec::probe_modules;
use crate::fixtures::ClosureHasher;

/// The cache file, alongside the daemon's `.tiderace-state.json` and gitignored with it.
const CACHE_FILE: &str = ".tiderace-subinterp.json";

/// One module's cached verdict, valid while its content hash is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeModule {
    pub hash: String,
    pub safe: bool,
}

/// The verdict map plus the probing that fills it. Front ends own the persistence decision; the
/// classification logic lives here so there is one copy of it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SafeSetCache {
    #[serde(default)]
    entries: BTreeMap<String, SafeModule>,
}

impl SafeSetCache {
    /// Build over an existing verdict map — the daemon's route, which keeps its own persistence.
    pub fn from_entries(entries: BTreeMap<String, SafeModule>) -> Self {
        Self { entries }
    }

    /// Hand the verdict map back, for a caller that persists it itself.
    pub fn into_entries(self) -> BTreeMap<String, SafeModule> {
        self.entries
    }

    /// Read the cache beside `root`. A missing or unparseable file is a cold start, not an error:
    /// the cost of being wrong here is one extra probe pass, and refusing to run because a cache
    /// file got truncated would be a far worse trade.
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(Self::path(root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write the cache beside `root`. Best-effort by design — a read-only or unwritable tree must
    /// still be runnable, just without the speedup, so the error is returned for the caller to
    /// ignore or log rather than raised into the run.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::path(root), json)
    }

    pub fn path(root: &Path) -> PathBuf {
        root.join(CACHE_FILE)
    }

    /// The sub-interpreter-safe subset of `modules`, probing only what is new or changed.
    ///
    /// Anything not positively known to be safe is treated as unsafe, which routes it to fork or
    /// subprocess. That direction is always sound — the cost of a false "unsafe" is lost
    /// parallelism, while a false "safe" is a crash inside an interpreter that cannot load the
    /// module.
    pub fn resolve(
        &mut self,
        python: &str,
        shim: &Path,
        root: &Path,
        modules: &[String],
    ) -> Result<HashSet<String>, String> {
        let to_probe: Vec<String> = modules
            .iter()
            .filter(|m| {
                self.entries
                    .get(*m)
                    .is_none_or(|rec| rec.hash != hash_file(root, m))
            })
            .cloned()
            .collect();

        if !to_probe.is_empty() {
            let verdicts = probe_modules(python, shim, root, &to_probe)?;
            for m in &to_probe {
                match verdicts.get(m) {
                    Some(Some(safe)) => {
                        self.entries.insert(
                            m.clone(),
                            SafeModule {
                                hash: hash_file(root, m),
                                safe: *safe,
                            },
                        );
                    }
                    // Undeterminable ⇒ do not cache. See the module docs.
                    _ => {
                        self.entries.remove(m);
                    }
                }
            }
        }

        Ok(modules
            .iter()
            .filter(|m| self.entries.get(*m).is_some_and(|r| r.safe))
            .cloned()
            .collect())
    }

    /// How many of `modules` would be probed right now — for callers that want to report the cost,
    /// and for tests that need to assert the cache actually prevented work rather than infer it
    /// from a timing.
    pub fn pending_probes(&self, root: &Path, modules: &[String]) -> usize {
        modules
            .iter()
            .filter(|m| {
                self.entries
                    .get(*m)
                    .is_none_or(|rec| rec.hash != hash_file(root, m))
            })
            .count()
    }
}

/// Hex content hash of `<root>/rel`; a sentinel for a missing or unreadable file, which therefore
/// always counts as changed and re-probes.
fn hash_file(root: &Path, rel: &str) -> String {
    match std::fs::read(root.join(rel)) {
        Ok(bytes) => {
            let digest = ClosureHasher::new().feed(&bytes).finish();
            let mut s = String::with_capacity(64);
            for b in digest.as_bytes() {
                s.push_str(&format!("{b:02x}"));
            }
            s
        }
        Err(_) => "missing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tiderace_safeset_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_cached_verdict_survives_a_round_trip_through_the_file() {
        let dir = temp("roundtrip");
        std::fs::write(dir.join("m.py"), "x = 1\n").unwrap();
        let mut cache = SafeSetCache::default();
        cache.entries.insert(
            "m.py".to_string(),
            SafeModule {
                hash: hash_file(&dir, "m.py"),
                safe: true,
            },
        );
        cache.save(&dir).expect("cache writes");

        let reloaded = SafeSetCache::load(&dir);
        assert_eq!(
            reloaded.pending_probes(&dir, &["m.py".to_string()]),
            0,
            "an unchanged module must not be re-probed after a reload — that is the whole point"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_a_module_invalidates_its_verdict() {
        let dir = temp("invalidate");
        std::fs::write(dir.join("m.py"), "x = 1\n").unwrap();
        let mut cache = SafeSetCache::default();
        cache.entries.insert(
            "m.py".to_string(),
            SafeModule {
                hash: hash_file(&dir, "m.py"),
                safe: true,
            },
        );
        assert_eq!(cache.pending_probes(&dir, &["m.py".to_string()]), 0);

        // A module that now imports numpy is a different question than the one we answered.
        std::fs::write(dir.join("m.py"), "import numpy\nx = 1\n").unwrap();
        assert_eq!(
            cache.pending_probes(&dir, &["m.py".to_string()]),
            1,
            "changed content must re-probe"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_corrupt_cache_file_is_a_cold_start_not_a_failure() {
        let dir = temp("corrupt");
        assert_eq!(SafeSetCache::load(&dir).entries.len(), 0, "missing file");
        std::fs::write(SafeSetCache::path(&dir), "{ not json").unwrap();
        assert_eq!(SafeSetCache::load(&dir).entries.len(), 0, "corrupt file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing positively known to be safe is reported safe, so an empty cache routes everything to
    /// the sound fallback rather than optimistically to the pool.
    #[test]
    fn unknown_modules_are_never_reported_safe() {
        let dir = temp("unknown");
        let cache = SafeSetCache::default();
        assert_eq!(
            cache.pending_probes(&dir, &["never_seen.py".to_string()]),
            1
        );
    }
}
