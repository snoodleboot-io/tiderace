//! The warm image for full parallel runs (TID-84): a persistent pool parent holding the imported
//! suite, from which every `RunFull` forks its workers. Dropped and relaunched when the tree's
//! files change, so a stale module is never executed. Unix only — the one place the daemon
//! says so.

#[cfg(unix)]
use crate::error::Result;
#[cfg(unix)]
use crate::tree_stamp::tree_stamp;
use crate::EngineHandler;

impl EngineHandler {
    /// The warm image for this run, taken out of `self` for the run's duration: reused while the
    /// tree is unchanged and the parent alive; otherwise dropped, and — for a full run — relaunched,
    /// which is the full import a full run pays anyway (TID-84). An impacted run on a changed tree
    /// gets `None`: it runs on the one-shot pool with its selective import (TID-75), which is
    /// cheaper than importing the whole tree into a new image it may not need.
    #[cfg(unix)]
    pub(crate) fn warm_pool(
        &mut self,
        launch: bool,
    ) -> Result<Option<engine_core::exec::WellspringPool>> {
        let stamp = tree_stamp(&self.root);
        if let Some(mut pool) = self.warm.take() {
            if self.warm_stamp == Some(stamp) && pool.is_alive() {
                return Ok(Some(pool));
            }
            drop(pool); // stale or dead: its parent exits
        }
        if !launch {
            return Ok(None);
        }
        let pool = engine_core::exec::WellspringPool::launch_persistent(
            &self.python,
            &self.shim,
            &self.root,
            true,
        )?;
        self.warm_stamp = Some(stamp);
        Ok(Some(pool))
    }

    /// The warm image's parent pid, when one is held.
    pub(crate) fn warm_pid(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            self.warm.as_ref().map(|p| p.pid())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Whether a warm image is currently held.
    pub fn is_warm(&self) -> bool {
        #[cfg(unix)]
        {
            self.warm.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}
