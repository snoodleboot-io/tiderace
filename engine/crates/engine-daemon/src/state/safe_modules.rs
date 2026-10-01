//! The sub-interpreter-safe module set (ADR-E015 TID-9, TID-11): probed once per module by
//! `engine_core`'s [`SafeSetCache`] and persisted in the daemon's state, so neither the daemon
//! nor the CLI re-probes an unchanged module (TID-35).

use std::collections::HashSet;

use engine_core::exec::SafeSetCache;

use crate::error::Result;
use crate::state::plan::PersistedState;
use crate::EngineHandler;

impl EngineHandler {
    /// The sub-interpreter-safe module set for `modules` (ADR-E015 TID-9 cache + TID-11).
    ///
    /// The classification and content-hash invalidation live in `engine_core`'s [`SafeSetCache`], so
    /// the CLI gets the same behaviour instead of re-probing every run (TID-35). The daemon keeps
    /// *persisting* the verdicts in its own state file, which it already writes.
    pub(crate) fn safe_set(
        &self,
        state: &mut PersistedState,
        modules: &[String],
    ) -> Result<HashSet<String>> {
        let mut cache = SafeSetCache::from_entries(std::mem::take(&mut state.safe_modules));
        let safe = cache.resolve(&self.python, &self.shim, &self.root, modules);
        state.safe_modules = cache.into_entries();
        Ok(safe?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use engine_core::testing::skip_live;

    use crate::state::plan::PersistedState;
    use crate::EngineHandler;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("repo root")
    }

    /// Sub-interp safety needs CPython 3.14 (`concurrent.interpreters`) + numpy for the unsafe case —
    /// gate the `safe_set` test on the fx venv.
    fn fx_venv() -> Option<String> {
        let p = repo_root().join(".tiderace-fx-venv/bin/python");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }

    #[test]
    fn safe_set_classifies_probes_once_and_caches() {
        let Some(python) = fx_venv() else {
            skip_live("`.tiderace-fx-venv` (CPython 3.14 + numpy) not present");
            return;
        };
        let dir = temp("safeset");
        std::fs::write(
            dir.join("test_pure.py"),
            "def test_a():\n    assert 1 == 1\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("test_np.py"),
            "import numpy\ndef test_n():\n    assert int(numpy.array([1]).sum()) == 1\n",
        )
        .unwrap();
        let handler = EngineHandler::new(
            python,
            repo_root().join("engine/py-shim/shim.py"),
            dir.clone(),
        );
        let mut state = PersistedState::default();
        let modules = vec!["test_pure.py".to_string(), "test_np.py".to_string()];

        let safe = handler.safe_set(&mut state, &modules).expect("safe_set");
        assert!(
            safe.contains("test_pure.py"),
            "pure module is sub-interp-safe"
        );
        assert!(!safe.contains("test_np.py"), "numpy module is not safe");
        assert_eq!(
            state.safe_modules.get("test_pure.py").map(|r| r.safe),
            Some(true)
        );
        assert_eq!(
            state.safe_modules.get("test_np.py").map(|r| r.safe),
            Some(false)
        );

        // Second call: verdicts are cached by content hash, so the result is stable (no re-probe needed).
        let hashes: Vec<String> = state
            .safe_modules
            .values()
            .map(|r| r.hash.clone())
            .collect();
        let safe2 = handler
            .safe_set(&mut state, &modules)
            .expect("safe_set cached");
        assert_eq!(safe, safe2);
        assert_eq!(
            hashes,
            state
                .safe_modules
                .values()
                .map(|r| r.hash.clone())
                .collect::<Vec<_>>(),
            "unchanged modules keep their cached verdict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tiderace_safeset_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
