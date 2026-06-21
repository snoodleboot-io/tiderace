//! `ForkPermit` — an RAII admission token from the [`crate::exec::MemoryGovernor`]. Holding one
//! means the governor has accounted for this in-flight fork against the RSS budget; dropping it
//! releases the budget (design 05 §6.3).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Proof that a fork has been admitted against the memory budget. Carries the estimated bytes it was
/// charged so the governor can reconcile against observed RSS later. When issued by a governor it
/// holds a release handle to the shared budget and **returns the charged bytes on drop** (RAII), so
/// concurrent admissions are naturally bounded by the available budget.
#[derive(Debug)]
pub struct ForkPermit {
    /// Bytes charged to the budget when this permit was issued.
    charged_bytes: u64,
    /// The governor's shared available-budget counter; `Some` when issued via `admit`, `None` for a
    /// bare permit. Dropping the permit adds `charged_bytes` back.
    release: Option<Arc<AtomicU64>>,
}

impl ForkPermit {
    /// Issue a bare permit charged `charged_bytes` (no budget release on drop). Kept so callers/tests
    /// can construct one without a governor.
    pub fn new(charged_bytes: u64) -> Self {
        Self {
            charged_bytes,
            release: None,
        }
    }

    /// Issue a permit bound to a governor's shared budget counter; its drop returns `charged_bytes`.
    pub(crate) fn admitted(charged_bytes: u64, release: Arc<AtomicU64>) -> Self {
        Self {
            charged_bytes,
            release: Some(release),
        }
    }

    /// The bytes this permit reserved.
    pub fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }
}

impl Drop for ForkPermit {
    fn drop(&mut self) {
        if let Some(budget) = &self.release {
            budget.fetch_add(self.charged_bytes, Ordering::SeqCst);
        }
    }
}
