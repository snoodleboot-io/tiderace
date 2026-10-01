//! The tree's test items, collected afresh when the tree stamp has moved and otherwise served
//! from the last collection (TID-101): collection is a walk of every test file, 52 ms of a
//! 390 ms `-k` round trip on a 5,600-node suite, and the daemon already takes the stamp per run
//! to validate the warm image.

use std::path::Path;

use engine_core::collection::{Collector, RegexCollector};
use engine_core::domain::TestItem;

use crate::error::Result;
#[cfg(unix)]
use crate::tree_stamp::tree_stamp;
use crate::EngineHandler;

impl EngineHandler {
    /// The tree's test items: collected afresh when the tree stamp has moved, else the last
    /// collection (TID-101). A stamp covers every `.py` and pytest config file under the root by
    /// path, size and mtime — an added, removed or edited test file changes it.
    pub(crate) fn collect(&mut self) -> Result<Vec<TestItem>> {
        #[cfg(unix)]
        {
            let stamp = tree_stamp(&self.root);
            if let Some((seen, items)) = &self.collected {
                if *seen == stamp {
                    return Ok(items.clone());
                }
            }
            let items = Self::collect_tree(&self.root)?;
            self.collected = Some((stamp, items.clone()));
            Ok(items)
        }
        #[cfg(not(unix))]
        {
            Self::collect_tree(&self.root)
        }
    }

    pub(crate) fn collect_tree(root: &Path) -> Result<Vec<TestItem>> {
        Ok(RegexCollector::new().collect(root)?)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::EngineHandler;

    /// TID-101: the collection is reused while the tree stamp holds, and taken again — with the
    /// change in it — the moment a test file is added, edited or removed.
    #[test]
    fn the_collection_follows_the_tree_stamp() {
        let dir = std::env::temp_dir().join(format!("tiderace_t101_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test_one.py");
        std::fs::write(&file, "def test_a():\n    assert True\n").unwrap();
        let mut h = EngineHandler::new("python3", "shim.py", &dir);
        let ids = |items: &[engine_core::domain::TestItem]| {
            items
                .iter()
                .map(|i| i.node_id.to_string())
                .collect::<Vec<_>>()
        };
        let first = h.collect().unwrap();
        assert_eq!(ids(&first), ["test_one.py::test_a"]);
        assert!(
            h.collected.is_some(),
            "the collection is kept with its stamp"
        );
        // Unchanged tree: the kept collection, not a walk.
        let (stamp_before, _) = h.collected.clone().unwrap();
        assert_eq!(ids(&h.collect().unwrap()), ids(&first));
        assert_eq!(h.collected.as_ref().unwrap().0, stamp_before);
        // An edit that grows the file moves the stamp (size, and mtime) and the collection with it.
        std::fs::write(
            &file,
            "def test_a():\n    assert True\n\ndef test_b():\n    assert True\n",
        )
        .unwrap();
        assert_eq!(
            ids(&h.collect().unwrap()),
            ["test_one.py::test_a", "test_one.py::test_b"]
        );
        assert_ne!(h.collected.as_ref().unwrap().0, stamp_before);
        // A removed file, likewise.
        std::fs::remove_file(&file).unwrap();
        assert!(h.collect().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
