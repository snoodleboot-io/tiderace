//! The content-addressed result cache's key (ADR-E004, TID-7): the node, the engine, the
//! interpreter, the platform, and the current content of every file the test's footprint names.

use engine_core::cache::{CacheKey, CacheKeyBuilder};

use crate::watch::content_hash;

use crate::EngineHandler;

impl EngineHandler {
    /// The platform term for the cache key — partitions the cache across OS/arch so a result never
    /// crosses platforms (ADR-E004 invalidation).
    pub(crate) fn platform() -> String {
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    }

    /// The interpreter's version (e.g. `"3.12.4"`), a cache-key term so a result computed under one
    /// Python is never served under another. Best-effort — `"unknown"` on failure (still consistent
    /// within a machine, just coarser sharing). Queried once per `run_impacted` (a single subprocess).
    pub(crate) fn python_version(&self) -> String {
        std::process::Command::new(&self.python)
            .arg("--version")
            .output()
            .ok()
            .map(|o| {
                let out = if o.stdout.is_empty() {
                    o.stderr
                } else {
                    o.stdout
                };
                String::from_utf8_lossy(&out)
                    .split_whitespace()
                    .last()
                    .unwrap_or("unknown")
                    .to_string()
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// The content-addressed [`CacheKey`] for `node` over `deps`' **current** content, or `None` if
    /// `deps` is empty (no recorded footprint ⇒ not soundly cacheable) or any dep is unreadable. Built
    /// the same way for `get` and `put`, so a hit ⟺ the executed-source closure is byte-identical to
    /// when the outcome was produced.
    pub(crate) fn cache_key(
        &self,
        node: &str,
        deps: &[String],
        py_version: &str,
    ) -> Option<CacheKey> {
        if deps.is_empty() {
            return None;
        }
        let mut b = CacheKeyBuilder::new(
            node,
            env!("CARGO_PKG_VERSION"),
            py_version,
            Self::platform(),
        );
        for dep in deps {
            let bytes = std::fs::read(self.root.join(dep)).ok()?; // unreadable dep ⇒ no sound key
            b.executed_source(dep.clone(), content_hash(&bytes));
        }
        Some(b.finish())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::EngineHandler;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tiderace_ck_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn cache_key_is_deterministic_node_python_and_content_sensitive() {
        let dir = temp("sens");
        std::fs::write(dir.join("src.py"), b"x = 1").unwrap();
        let h = EngineHandler::new("python3", "shim.py", dir.clone());
        let deps = vec!["src.py".to_string()];

        let k = h.cache_key("t.py::a", &deps, "3.12").unwrap();
        assert_eq!(
            k,
            h.cache_key("t.py::a", &deps, "3.12").unwrap(),
            "same inputs → same key"
        );
        assert_ne!(
            k,
            h.cache_key("t.py::b", &deps, "3.12").unwrap(),
            "node partitions the key"
        );
        assert_ne!(
            k,
            h.cache_key("t.py::a", &deps, "3.13").unwrap(),
            "python version partitions the key"
        );

        std::fs::write(dir.join("src.py"), b"x = 2").unwrap();
        assert_ne!(
            k,
            h.cache_key("t.py::a", &deps, "3.12").unwrap(),
            "a content change misses the cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_key_none_when_unsound() {
        let dir = temp("unsound");
        let h = EngineHandler::new("python3", "shim.py", dir.clone());
        assert!(
            h.cache_key("t.py::a", &[], "3.12").is_none(),
            "no recorded deps → no sound key"
        );
        assert!(
            h.cache_key("t.py::a", &["missing.py".to_string()], "3.12")
                .is_none(),
            "an unreadable dep → no key (run instead)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
