use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Compute SHA256 hash of a file's contents
pub fn hash_file(path: &Path) -> Result<String> {
    let contents = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    Ok(hex::encode(hasher.finalize()))
}

/// Scan all Python files in a directory tree and return path -> hash map
pub fn hash_all_python_files(root: &Path) -> Result<HashMap<String, String>> {
    let mut hashes = HashMap::new();

    for entry in WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            path.extension().is_some_and(|ext| ext == "py")
                && !path.components().any(|c| {
                    let s = c.as_os_str().to_string_lossy();
                    s == ".git"
                        || s == "__pycache__"
                        || s == ".venv"
                        || s == "venv"
                        || s == "node_modules"
                })
        })
    {
        let path = entry.path();
        let hash = hash_file(path)?;
        // Normalize a leading "./" so paths from walking "." match the relative
        // paths the collector and coverage report (e.g. "src/x.py"), which keeps
        // change detection and the per-test dependency graph aligned.
        let raw = path.to_string_lossy();
        let path_str = raw.strip_prefix("./").unwrap_or(&raw).to_string();
        hashes.insert(path_str, hash);
    }

    Ok(hashes)
}

/// Determine which files have changed compared to stored hashes
pub fn find_changed_files(
    current: &HashMap<String, String>,
    db: &crate::db::Database,
) -> Result<Vec<String>> {
    let mut changed = Vec::new();

    for (path, current_hash) in current {
        match db.get_file_hash(path)? {
            None => {
                // Never seen before — treat as changed
                changed.push(path.clone());
            }
            Some(stored_hash) if stored_hash != *current_hash => {
                changed.push(path.clone());
            }
            _ => {}
        }
    }

    Ok(changed)
}

/// `(changed relative paths, (path, hash) pairs to persist)`.
type ChangeSet = (Vec<String>, Vec<(String, String)>);

/// Incremental change detection for watch mode: hash each of `paths` (relativized
/// to `cwd` and `./`-stripped so keys match the database) and return the paths
/// whose content differs from the stored hash, plus the `(path, hash)` pairs to
/// persist. Unreadable/deleted paths are skipped. Avoids re-hashing the whole tree.
pub fn detect_changes(
    paths: &[PathBuf],
    cwd: &Path,
    db: &crate::db::Database,
) -> Result<ChangeSet> {
    let mut changed = Vec::new();
    let mut updates = Vec::new();
    for p in paths {
        let rel = p.strip_prefix(cwd).unwrap_or(p).to_string_lossy();
        let rel = rel.strip_prefix("./").unwrap_or(&rel).to_string();
        if let Ok(h) = hash_file(p) {
            if db.get_file_hash(&rel)?.as_deref() != Some(h.as_str()) {
                changed.push(rel.clone());
                updates.push((rel, h));
            }
        }
    }
    Ok((changed, updates))
}

/// Persist all current hashes into the database
pub fn save_hashes(hashes: &HashMap<String, String>, db: &crate::db::Database) -> Result<()> {
    for (path, hash) in hashes {
        db.save_file_hash(path, hash)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn detect_changes_flags_new_and_modified_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("state.db")).unwrap();
        let f = dir.path().join("mod.py");
        std::fs::write(&f, b"x = 1\n").unwrap();

        // First sight: new file => changed, with an update to persist.
        let (changed, updates) = detect_changes(std::slice::from_ref(&f), dir.path(), &db).unwrap();
        assert_eq!(changed, vec!["mod.py".to_string()]);
        assert_eq!(updates.len(), 1);
        // Persist, then re-check unchanged => nothing.
        save_hashes(&updates.iter().cloned().collect(), &db).unwrap();
        let (changed, _) = detect_changes(std::slice::from_ref(&f), dir.path(), &db).unwrap();
        assert!(changed.is_empty());

        // Modify => changed again.
        std::fs::write(&f, b"x = 2\n").unwrap();
        let (changed, _) = detect_changes(std::slice::from_ref(&f), dir.path(), &db).unwrap();
        assert_eq!(changed, vec!["mod.py".to_string()]);

        // A path that doesn't exist is skipped, not an error.
        let (changed, _) = detect_changes(&[dir.path().join("gone.py")], dir.path(), &db).unwrap();
        assert!(changed.is_empty());
    }
}
