use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
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
        let path_str = path.to_string_lossy().to_string();
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

/// Persist all current hashes into the database
pub fn save_hashes(hashes: &HashMap<String, String>, db: &crate::db::Database) -> Result<()> {
    for (path, hash) in hashes {
        db.save_file_hash(path, hash)?;
    }
    Ok(())
}
