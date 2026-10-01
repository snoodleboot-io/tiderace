//! A digest of the tree's test and config files, cheap enough to take per run (TID-84, TID-101):
//! what decides whether a warm image, or a cached collection, still describes the tree.

use std::path::Path;

/// A digest of every `.py` file and pytest config file under the root — path, mtime, size —
/// cheap enough to take per run. It is what decides whether the warm image still describes
/// the tree (TID-84): the shim reads the config (`addopts`, markers) at start-up, so a config
/// edit stales the image exactly as a source edit does.
pub(crate) fn tree_stamp(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    fn walk(dir: &Path, root: &Path, h: &mut std::collections::hash_map::DefaultHasher) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !engine_core::collection::SKIP_DIRS.contains(&name.as_ref()) {
                    walk(&path, root, h);
                }
            } else if name.ends_with(".py")
                || engine_core::collection::CONFIG_FILES.contains(&name.as_ref())
            {
                if let Ok(meta) = entry.metadata() {
                    path.strip_prefix(root).unwrap_or(&path).hash(h);
                    meta.len().hash(h);
                    if let Ok(m) = meta.modified() {
                        m.hash(h);
                    }
                }
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    walk(root, root, &mut h);
    h.finish()
}
