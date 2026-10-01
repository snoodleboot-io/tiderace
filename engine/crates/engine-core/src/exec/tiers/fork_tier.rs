//! The fork tier for one run (TID-118): one imported image — a daemon's warm one, or this run's
//! own when `shared_import` is on — forked per lane, or one wellspring per lane when it is off.

use std::path::Path;

use crate::error::Result;
use crate::exec::knobs::RunKnobs;
use crate::exec::process::ShimTarget;
use crate::exec::tier::{LaneSeed, TierFactory, WarmImage};
use crate::exec::tiers::fork::ForkWorker;
use crate::exec::tiers::pool::{PooledTransport, PooledWorker, WellspringPool};
use crate::exec::worker::Worker;
use crate::runner::{RunNotes, RunPlan, WorkerCount};

pub struct ForkTier<'a> {
    target: ShimTarget,
    knobs: RunKnobs,
    shared_import: bool,
    workers: WorkerCount,
    memory_limit_mb: Option<u64>,
    /// A daemon's image, borrowed for the run (TID-84).
    warm: Option<&'a mut WellspringPool>,
    /// This run's own image, launched in [`prepare`](TierFactory::prepare) and dropped with the
    /// factory — after every lane has been joined, so its parent exits only once every worker
    /// connection has closed.
    owned: Option<WellspringPool>,
}

impl<'a> ForkTier<'a> {
    pub fn new(target: ShimTarget, plan: &RunPlan, knobs: RunKnobs, warm: WarmImage<'a>) -> Self {
        Self {
            target,
            knobs,
            shared_import: plan.fork.shared_import,
            workers: plan.workers,
            memory_limit_mb: plan.memory_limit_mb,
            warm: warm.into_pool(),
            owned: None,
        }
    }

    fn pool(&mut self) -> Option<&mut WellspringPool> {
        match self.warm.as_deref_mut() {
            Some(w) => Some(w),
            None => self.owned.as_mut(),
        }
    }
}

impl TierFactory for ForkTier<'_> {
    fn prepare(&mut self, lanes: usize, modules: &Path, notes: &mut RunNotes) -> Result<usize> {
        // TID-4: one imported image, forked per lane. Stood up before any lane starts so the
        // import is finished — and paid once — first. Launched with restore unconditionally,
        // exactly as `ForkWorker::launch_optimistic` does: it costs nothing when the ladder is
        // off, and it makes the unsound combination — in-process execution with no snapshot —
        // unreachable rather than merely unused. Persistent rather than sized at launch
        // (TID-106): the image is up, and its resident size known, before the lane count is
        // decided.
        if self.warm.is_none() && self.shared_import {
            self.owned = Some(WellspringPool::launch_persistent_selected(
                &self.target.python,
                &self.target.shim,
                &self.target.root,
                true,
                Some(modules),
            )?);
        }
        let (explicit, limit) = (self.workers.is_explicit(), self.memory_limit_mb);
        let Some(pool) = self.pool() else {
            return Ok(lanes);
        };
        // The persistent parent — warm or this run's own — forks the workers now, off its
        // imported image; how many is the lane count, capped by what memory allows (TID-106).
        let sizing = crate::runner::workers_by_memory(
            lanes,
            explicit,
            crate::runner::process_rss_bytes(pool.pid()),
            crate::runner::available_memory_bytes(),
            limit.map(|mb| mb << 20),
        );
        if let Some(note) = &sizing.note {
            notes.push(note.clone());
        }
        pool.spawn_workers(sizing.workers)?;
        Ok(sizing.workers)
    }

    fn lane(&mut self, _index: usize, modules: &Path) -> Result<Box<dyn LaneSeed>> {
        let knobs = self.knobs.clone();
        if let Some(transport) = self.pool().and_then(|p| p.take_worker()) {
            return Ok(Box::new(PooledLane { transport, knobs }));
        }
        Ok(Box::new(WellspringLane {
            target: self.target.clone(),
            knobs,
            modules: modules.to_path_buf(),
        }))
    }
}

/// A lane on a worker forked off the pool's image: its connection is already open.
struct PooledLane {
    transport: PooledTransport,
    knobs: RunKnobs,
}

impl LaneSeed for PooledLane {
    fn start(self: Box<Self>) -> Result<Box<dyn Worker>> {
        Ok(Box::new(
            PooledWorker::new(self.transport, self.knobs.deadline_ms).with_knobs(self.knobs),
        ))
    }
}

/// A lane with a wellspring of its own, launched on the lane's thread so N lanes import in
/// parallel.
struct WellspringLane {
    target: ShimTarget,
    knobs: RunKnobs,
    modules: std::path::PathBuf,
}

impl LaneSeed for WellspringLane {
    fn start(self: Box<Self>) -> Result<Box<dyn Worker>> {
        // The ladder and restore are launched together or not at all — see
        // `ForkWorker::launch_optimistic`.
        Ok(Box::new(
            ForkWorker::launch_target(
                &self.target,
                self.knobs.optimistic_no_fork,
                Some(&self.modules),
                self.knobs.deadline_ms,
            )?
            .with_knobs(self.knobs),
        ))
    }
}
