use std::path::Path;

use crate::domain::TestItem;
use crate::error::Result;

/// Discovers tests under a root directory. Implementations must not import Python — discovery is
/// a pure scan so the engine pays zero interpreter startup to collect (a trait seam, [ADR-E005]).
///
/// Node ids in the returned items are relative to `root`, which is also the directory placed on
/// the Wellspring's `sys.path` at execution time.
pub trait Collector {
    fn collect(&self, root: &Path) -> Result<Vec<TestItem>>;
}
