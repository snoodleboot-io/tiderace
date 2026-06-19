//! `ForkPermit` — an RAII admission token from the [`crate::exec::MemoryGovernor`]. Holding one
//! means the governor has accounted for this in-flight fork against the RSS budget; dropping it
//! releases the budget (design 05 §6.3).
//!
//! **Contract seam.** Shape frozen here; the release-on-drop accounting is wired by Lane FALLBACK
//! (subagent fb-governor) when it implements the governor's internals.

/// Proof that a fork has been admitted against the memory budget. Carries the estimated bytes it was
/// charged so the governor can reconcile against observed RSS later.
#[derive(Debug)]
pub struct ForkPermit {
    /// Bytes charged to the budget when this permit was issued.
    charged_bytes: u64,
}

impl ForkPermit {
    /// Issue a permit charged `charged_bytes` against the budget. (Constructed by the governor.)
    pub fn new(charged_bytes: u64) -> Self {
        Self { charged_bytes }
    }

    /// The bytes this permit reserved.
    pub fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }
}
