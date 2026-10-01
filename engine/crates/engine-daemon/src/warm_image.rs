//! The warm image for full parallel runs (TID-84): a persistent pool parent holding the imported
//! suite, from which every `RunFull` forks its workers. Dropped and relaunched when the tree's
//! files change, so a stale module is never executed. Fork-only, so Unix-only — and this file is
//! the one place the daemon says so: elsewhere the image is a plain type that may be empty.

use std::path::Path;

use engine_core::exec::Selection;

use crate::error::Result;
#[cfg(unix)]
use crate::tree_stamp::tree_stamp;

/// The image the daemon holds between runs, with the tree stamp it was built under.
pub(crate) struct WarmImage {
    #[cfg(unix)]
    pool: Option<engine_core::exec::WellspringPool>,
    #[cfg(unix)]
    stamp: Option<u64>,
}

/// The image taken out of [`WarmImage`] for one run's duration, to be put back afterwards.
pub(crate) struct HeldImage {
    #[cfg(unix)]
    pool: Option<engine_core::exec::WellspringPool>,
}

impl WarmImage {
    /// No image yet.
    pub(crate) fn none() -> Self {
        Self {
            #[cfg(unix)]
            pool: None,
            #[cfg(unix)]
            stamp: None,
        }
    }

    /// Whether an image is currently held.
    pub(crate) fn is_held(&self) -> bool {
        #[cfg(unix)]
        {
            self.pool.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// The image's parent pid, when one is held.
    pub(crate) fn pid(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            self.pool.as_ref().map(|p| p.pid())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// The image for this run, taken out for the run's duration: reused while the tree is
    /// unchanged and the parent alive; otherwise dropped, and — when `launch` — relaunched, which
    /// is the full import a full run pays anyway (TID-84). An impacted run on a changed tree
    /// passes `launch = false` and gets none: it runs on the one-shot pool with its selective
    /// import (TID-75), which is cheaper than importing the whole tree into a new image it may
    /// not need. Put it back with [`put_back`](Self::put_back) whatever the run's outcome.
    pub(crate) fn take_for_run(
        &mut self,
        python: &str,
        shim: &Path,
        root: &Path,
        launch: bool,
    ) -> Result<HeldImage> {
        #[cfg(unix)]
        {
            let stamp = tree_stamp(root);
            if let Some(mut pool) = self.pool.take() {
                if self.stamp == Some(stamp) && pool.is_alive() {
                    return Ok(HeldImage { pool: Some(pool) });
                }
                drop(pool); // stale or dead: its parent exits
            }
            if !launch {
                return Ok(HeldImage { pool: None });
            }
            let pool =
                engine_core::exec::WellspringPool::launch_persistent(python, shim, root, true)?;
            self.stamp = Some(stamp);
            Ok(HeldImage { pool: Some(pool) })
        }
        #[cfg(not(unix))]
        {
            let _ = (python, shim, root, launch);
            Ok(HeldImage::none())
        }
    }

    /// The image back from a run, to fork the next run from.
    pub(crate) fn put_back(&mut self, held: HeldImage) {
        #[cfg(unix)]
        {
            self.pool = held.pool;
        }
        #[cfg(not(unix))]
        {
            let _ = held;
        }
    }
}

impl HeldImage {
    /// No image for this run: the one-shot pool imports for itself.
    pub(crate) fn none() -> Self {
        Self {
            #[cfg(unix)]
            pool: None,
        }
    }

    /// Hand the run's selection (TID-90) to the image's workers, who apply it after the fork.
    /// `false` when there is no image: the selection must then reach the one-shot pool the way
    /// `tiderace run` hands it over, through the environment.
    pub(crate) fn set_selection(&mut self, selection: Option<Selection>) -> bool {
        #[cfg(unix)]
        {
            match self.pool.as_mut() {
                Some(p) => {
                    p.set_selection(selection);
                    true
                }
                None => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = selection;
            false
        }
    }

    /// The handle the runner forks from.
    pub(crate) fn for_runner(&mut self) -> engine_core::exec::WarmImage<'_> {
        #[cfg(unix)]
        {
            match self.pool.as_mut() {
                Some(p) => engine_core::exec::WarmImage::of(p),
                None => engine_core::exec::WarmImage::none(),
            }
        }
        #[cfg(not(unix))]
        {
            engine_core::exec::WarmImage::none()
        }
    }
}
