//! Debounced recursive file watcher for `riptide watch` (ADR-009, stage B).
//!
//! Wraps `notify-debouncer-full` so editor atomic-saves (write-temp + rename) are
//! stitched into sane events, and yields a deduplicated batch of changed `.py`
//! paths per quiet window. Artifact dirs and riptide's own state are ignored so a
//! test run cannot retrigger itself.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};

/// Path components that disqualify a change (build/vcs/venv/artifacts).
const IGNORED_COMPONENTS: &[&str] = &[
    "__pycache__",
    ".git",
    ".venv",
    "venv",
    "node_modules",
    ".riptide-coverage",
];

/// Watch `root` recursively; invoke `on_batch` with a deduplicated set of changed
/// `.py` files on each ~250ms quiet window. Blocks until `on_batch` returns `Err`.
pub fn watch_loop<F>(root: &Path, mut on_batch: F) -> Result<()>
where
    F: FnMut(&[PathBuf]) -> Result<()>,
{
    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(250), None, tx)
        .context("failed to create file-system debouncer")?;
    debouncer
        .watcher()
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", root.display()))?;
    debouncer.cache().add_root(root, RecursiveMode::Recursive);

    for result in rx {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                for e in errors {
                    eprintln!("  watch error: {e}");
                }
                continue;
            }
        };

        let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
        for event in &events {
            collect_relevant_paths(event, &mut changed);
        }
        if changed.is_empty() {
            continue;
        }
        let batch: Vec<PathBuf> = changed.into_iter().collect();
        on_batch(&batch).context("watch batch handler failed")?;
    }
    Ok(())
}

fn collect_relevant_paths(event: &DebouncedEvent, out: &mut BTreeSet<PathBuf>) {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any => {
            for path in &event.paths {
                if is_relevant_python_path(path) {
                    out.insert(path.clone());
                }
            }
        }
        _ => {}
    }
}

/// True only for `*.py` files outside ignored directories and not riptide state.
fn is_relevant_python_path(path: &Path) -> bool {
    if path.components().any(|c| {
        let s = c.as_os_str();
        IGNORED_COMPONENTS.iter().any(|ig| s == *ig)
    }) {
        return false;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.starts_with(".riptide.db") {
            return false;
        }
    }
    matches!(path.extension().and_then(|e| e.to_str()), Some("py"))
}

#[cfg(test)]
mod tests {
    use super::is_relevant_python_path;
    use std::path::Path;

    #[test]
    fn accepts_project_python_files() {
        assert!(is_relevant_python_path(Path::new("tests/test_a.py")));
        assert!(is_relevant_python_path(Path::new("src/pkg/mod.py")));
        assert!(is_relevant_python_path(Path::new("conftest.py")));
    }

    #[test]
    fn rejects_non_python_and_artifacts() {
        assert!(!is_relevant_python_path(Path::new("README.md")));
        assert!(!is_relevant_python_path(Path::new("src/mod.pyc")));
        assert!(!is_relevant_python_path(Path::new("__pycache__/mod.py")));
        assert!(!is_relevant_python_path(Path::new(".venv/lib/x.py")));
        assert!(!is_relevant_python_path(Path::new(".git/hooks/x.py")));
        assert!(!is_relevant_python_path(Path::new(
            ".riptide-coverage/c.py"
        )));
        // riptide's own state files (incl. sqlite -wal/-shm siblings).
        assert!(!is_relevant_python_path(Path::new(".riptide.db")));
    }
}
